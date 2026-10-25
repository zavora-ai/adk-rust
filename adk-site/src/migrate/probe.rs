//! Probes (spec 3.5 §7.8 "Probes", §7.8.3 "Probes under constant change", §7.11, A-I4, S5):
//! sampled page digests from the host and the public origin against the signed release, the
//! site's status, its certificate's expiry, its DNS records against the recorded set and its
//! security headers. A [`Drift`] is what the Guardian acts on.
//!
//! | Probe | Drift when |
//! |-------|-----------|
//! | Checkpoint | The host's checkpoint is missing or not signed by a trusted release key |
//! | Pages | A sampled file's digest matches neither the release the checkpoint names nor the one after it (one release of lag), on the host or at the origin |
//! | Absent paths | A legacy runtime path (`/wp-login.php`, `/xmlrpc.php`, …) that no release lists is served |
//!
//! Status, certificate, DNS and headers are reported, not drift. Every probe beyond fetching
//! published documents runs only on a proven domain (A-I9), so a [`Prober`] needs a
//! [`ProvenDomain`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use adk_auth::signed_audit::NoteVerifier;
use adk_core::audit_log::AuditRecorder;
use adk_core::{ActingContext, Clock, SystemClock};
use awp_types::{Digest, FileEntry, ReleaseManifest};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use url::Url;

use super::audit::record;
use super::claim::{DnsResolver, ProvenDomain, RecordKind};
use super::error::{MigrateError, Result};
use super::http::{FetchOptions, Fetched, HttpClient};
use crate::host::{HostTiles, SiteHost};
use crate::log::{
    CHECKPOINT_PATH, TileProofs, VerifiedCheckpoint, verify_checkpoint, verify_release_inclusion,
};
use crate::release::{file_digest, manifest_path};

/// Paths of legacy runtimes that a static site never serves.
pub const DEFAULT_ABSENT_PATHS: &[&str] = &[
    "/wp-login.php",
    "/xmlrpc.php",
    "/wp-admin/",
    "/wp-config.php",
    "/.env",
    "/index.php",
    "/wp-content/uploads/cache.php",
];

/// Security headers the origin's home page is checked for.
pub const DEFAULT_HEADERS: &[&str] = &[
    "strict-transport-security",
    "content-security-policy",
    "x-content-type-options",
    "referrer-policy",
];

/// A DNS record set the site expects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DnsExpectation {
    /// The name.
    pub name: String,
    /// The record type.
    pub kind: RecordKind,
    /// The values recorded when the site was set up.
    pub values: BTreeSet<String>,
}

/// What a prober checks.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeConfig {
    /// The site's domain.
    pub domain: String,
    /// Its public origin, such as `https://example.com`.
    pub public_origin: Url,
    /// Files sampled per probe (default 20).
    pub sample: usize,
    /// The recorded DNS records.
    pub dns: Vec<DnsExpectation>,
    /// Days before expiry the certificate is reported as expiring (default 14).
    pub certificate_warning_days: i64,
    /// Headers the home page must send.
    pub headers: Vec<String>,
    /// Paths that must not be served.
    pub absent_paths: Vec<String>,
    /// A sampling seed; by default the probe time.
    pub seed: Option<u64>,
}

impl ProbeConfig {
    /// Defaults for `domain` served at `public_origin`.
    pub fn new(domain: impl Into<String>, public_origin: Url) -> Self {
        Self {
            domain: domain.into(),
            public_origin,
            sample: 20,
            dns: Vec::new(),
            certificate_warning_days: 14,
            headers: DEFAULT_HEADERS.iter().map(|h| (*h).to_string()).collect(),
            absent_paths: DEFAULT_ABSENT_PATHS.iter().map(|p| (*p).to_string()).collect(),
            seed: None,
        }
    }
}

/// Where drift was seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftLocation {
    /// What the host holds.
    Host,
    /// What the public origin serves.
    Origin,
}

/// What kind of drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DriftKind {
    /// A file differs from every release in the window.
    Changed,
    /// A released file is not served.
    Missing,
    /// A file no release lists is served.
    Unexpected,
    /// The checkpoint, a manifest or the log does not verify.
    Unverified,
}

/// A served state that no signed release in the window explains (spec 3.5 A-I4): the input of
/// the Guardian's `site.drift.detected` (S5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Drift {
    /// Where.
    pub location: DriftLocation,
    /// What kind.
    pub kind: DriftKind,
    /// The path.
    pub path: String,
    /// The digests the window's releases list for the path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected: Vec<Digest>,
    /// The digest found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub found: Option<Digest>,
    /// The releases in the window.
    pub window: Vec<u64>,
    /// What the probe saw.
    pub detail: String,
}

/// An HTTP status the origin answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusProbe {
    /// The path.
    pub path: String,
    /// The status, when one came.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// The error, when none came.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The origin's TLS certificate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CertificateProbe {
    /// When it expires.
    pub not_after: DateTime<Utc>,
    /// Whole days left.
    pub days_left: i64,
    /// Whether it expires within the warning window.
    pub expiring: bool,
}

impl CertificateProbe {
    /// Reads a DER certificate's expiry at `now`, warning `warning_days` ahead.
    ///
    /// # Errors
    ///
    /// [`MigrateError::Parse`] when the certificate does not parse.
    pub fn from_der(der: &[u8], now: DateTime<Utc>, warning_days: i64) -> Result<Self> {
        let (_, certificate) = x509_parser::parse_x509_certificate(der)
            .map_err(|e| MigrateError::parse("certificate", e))?;
        let not_after = DateTime::from_timestamp(certificate.validity().not_after.timestamp(), 0)
            .ok_or_else(|| MigrateError::parse("certificate", "expiry out of range"))?;
        let days_left = (not_after - now).num_days();
        Ok(Self { not_after, days_left, expiring: days_left <= warning_days })
    }
}

/// A DNS record set against the recorded one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DnsProbe {
    /// The name.
    pub name: String,
    /// The record type.
    pub kind: RecordKind,
    /// The recorded values.
    pub expected: BTreeSet<String>,
    /// The values found.
    pub found: BTreeSet<String>,
    /// Whether they match.
    pub matches: bool,
}

/// One probe's findings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeReport {
    /// When it ran.
    pub probed_at: DateTime<Utc>,
    /// The release the checkpoint named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<u64>,
    /// The releases a served file may match.
    pub window: Vec<u64>,
    /// Files compared, on the host and at the origin.
    pub files_checked: usize,
    /// Drift found.
    pub drift: Vec<Drift>,
    /// Statuses the origin answered.
    pub status: Vec<StatusProbe>,
    /// The certificate, on `https`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate: Option<CertificateProbe>,
    /// DNS record sets.
    pub dns: Vec<DnsProbe>,
    /// Required headers the home page does not send.
    pub missing_headers: Vec<String>,
    /// What the probe could not check.
    pub problems: Vec<String>,
}

impl ProbeReport {
    /// Whether anything drifted.
    pub fn drift_detected(&self) -> bool {
        !self.drift.is_empty()
    }
}

/// Probes a site against its signed releases (S5).
pub struct Prober {
    host: Arc<dyn SiteHost>,
    http: Arc<HttpClient>,
    dns: Arc<dyn DnsResolver>,
    trusted: Vec<NoteVerifier>,
    config: ProbeConfig,
    clock: Arc<dyn Clock>,
    audit: Option<(Arc<dyn AuditRecorder>, ActingContext)>,
}

impl std::fmt::Debug for Prober {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prober").field("domain", &self.config.domain).finish_non_exhaustive()
    }
}

/// A release's full file set, resolved through its delta chain from the host.
async fn full_files(
    host: &dyn SiteHost,
    seq: u64,
) -> Result<(ReleaseManifest, BTreeMap<String, FileEntry>)> {
    let read = |bytes: Option<bytes::Bytes>, at: u64| -> Result<ReleaseManifest> {
        let bytes = bytes.ok_or_else(|| {
            MigrateError::parse(format!("release {at}"), "the host does not hold its manifest")
        })?;
        serde_json::from_slice(&bytes).map_err(|e| MigrateError::parse(format!("release {at}"), e))
    };
    let top = read(host.fetch(&manifest_path(seq)).await?, seq)?;
    let mut chain = vec![top.clone()];
    while let Some(base) = chain.last().and_then(|m| m.base) {
        if chain.len() > 200 {
            return Err(MigrateError::parse(format!("release {seq}"), "delta chain too long"));
        }
        chain.push(read(host.fetch(&manifest_path(base)).await?, base)?);
    }
    let mut files = BTreeMap::new();
    for manifest in chain.iter().rev() {
        files = if manifest.base.is_none() {
            manifest.files.clone()
        } else {
            manifest.apply_to(&files)
        };
    }
    Ok((top, files))
}

impl Prober {
    /// A prober for the proven domain.
    ///
    /// # Errors
    ///
    /// [`MigrateError::NotProven`] when `proven` does not cover `config.domain` (A-I9).
    pub fn new(
        host: Arc<dyn SiteHost>,
        http: Arc<HttpClient>,
        dns: Arc<dyn DnsResolver>,
        trusted: Vec<NoteVerifier>,
        proven: &ProvenDomain,
        config: ProbeConfig,
    ) -> Result<Self> {
        if !proven.covers(&config.domain) {
            return Err(MigrateError::NotProven {
                domain: config.domain.clone(),
                action: "probing".into(),
            });
        }
        Ok(Self { host, http, dns, trusted, config, clock: Arc::new(SystemClock), audit: None })
    }

    /// Stamps reports with `clock`.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Records `site.drift.detected` in `recorder` as `acting`.
    #[must_use]
    pub fn with_audit(mut self, recorder: Arc<dyn AuditRecorder>, acting: ActingContext) -> Self {
        self.audit = Some((recorder, acting));
        self
    }

    async fn checkpoint(&self) -> Result<VerifiedCheckpoint> {
        let note = self
            .host
            .fetch(CHECKPOINT_PATH)
            .await?
            .ok_or_else(|| MigrateError::parse("checkpoint", "the host holds no checkpoint"))?;
        Ok(verify_checkpoint(&String::from_utf8_lossy(&note), &self.config.domain, &self.trusted)?)
    }

    /// The verified full file set of release `seq` under `checkpoint`.
    async fn release(
        &self,
        checkpoint: &VerifiedCheckpoint,
        seq: u64,
    ) -> Result<BTreeMap<String, FileEntry>> {
        let (_, files) = full_files(self.host.as_ref(), seq).await?;
        let manifest = self.host.fetch(&manifest_path(seq)).await?.unwrap_or_default();
        let tiles = HostTiles(self.host.as_ref());
        let proofs = TileProofs::new(&tiles, checkpoint.size);
        let entry = proofs.entry(seq - 1).await?;
        let proof = proofs.inclusion_proof(seq - 1).await?;
        verify_release_inclusion(checkpoint, &entry, &proof, seq, &file_digest(&manifest))?;
        Ok(files)
    }

    fn origin_url(&self, path: &str) -> Option<Url> {
        let served = path.strip_suffix("index.html").unwrap_or(path);
        self.config.public_origin.join(served).ok()
    }

    async fn origin_fetch(&self, url: &Url) -> Result<Fetched> {
        self.http.fetch(url, &FetchOptions::crawler()).await
    }

    /// Runs one probe.
    ///
    /// # Errors
    ///
    /// None for drift, which is reported; errors only when the report cannot be recorded.
    pub async fn probe(&self) -> Result<ProbeReport> {
        let now = self.clock.now();
        let mut report = ProbeReport {
            probed_at: now,
            checkpoint: None,
            window: Vec::new(),
            files_checked: 0,
            drift: Vec::new(),
            status: Vec::new(),
            certificate: None,
            dns: Vec::new(),
            missing_headers: Vec::new(),
            problems: Vec::new(),
        };
        let mut window: BTreeMap<u64, BTreeMap<String, FileEntry>> = BTreeMap::new();
        match self.checkpoint().await {
            Ok(checkpoint) => {
                report.checkpoint = Some(checkpoint.size);
                match self.release(&checkpoint, checkpoint.size).await {
                    Ok(files) => {
                        window.insert(checkpoint.size, files);
                    }
                    Err(error) => report.drift.push(Drift {
                        location: DriftLocation::Host,
                        kind: DriftKind::Unverified,
                        path: manifest_path(checkpoint.size),
                        expected: Vec::new(),
                        found: None,
                        window: vec![checkpoint.size],
                        detail: error.to_string(),
                    }),
                }
            }
            Err(error) => report.drift.push(Drift {
                location: DriftLocation::Host,
                kind: DriftKind::Unverified,
                path: format!("/{CHECKPOINT_PATH}"),
                expected: Vec::new(),
                found: None,
                window: Vec::new(),
                detail: error.to_string(),
            }),
        }
        let Some((&seq, files)) = window.iter().next() else {
            return self.finish(report).await;
        };
        let files = files.clone();
        let seed = self
            .config
            .seed
            .unwrap_or_else(|| now.timestamp_nanos_opt().unwrap_or_default() as u64);
        let mut paths: Vec<&String> =
            files.keys().filter(|p| !p.starts_with("/.well-known/awp/")).collect();
        paths.sort_by_key(|p| {
            let mut hasher = Sha256::new();
            hasher.update(seed.to_le_bytes());
            hasher.update(p.as_bytes());
            let rank: [u8; 32] = hasher.finalize().into();
            (p.as_str() != "/index.html", rank)
        });
        paths.truncate(self.config.sample.max(1));
        let mut observed: Vec<(DriftLocation, String, Option<Digest>)> = Vec::new();
        for path in &paths {
            let on_host = self.host.fetch(path).await?;
            observed.push((
                DriftLocation::Host,
                (*path).clone(),
                on_host.as_deref().map(file_digest),
            ));
            if let Some(url) = self.origin_url(path) {
                match self.origin_fetch(&url).await {
                    Ok(fetched) => {
                        report.status.push(StatusProbe {
                            path: (*path).clone(),
                            status: Some(fetched.status),
                            error: None,
                        });
                        let digest = (fetched.status == 200).then(|| file_digest(&fetched.body));
                        observed.push((DriftLocation::Origin, (*path).clone(), digest));
                    }
                    Err(error) => report.status.push(StatusProbe {
                        path: (*path).clone(),
                        status: None,
                        error: Some(error.to_string()),
                    }),
                }
            }
        }
        report.files_checked = observed.len();
        let explained = |window: &BTreeMap<u64, BTreeMap<String, FileEntry>>,
                         path: &str,
                         digest: &Option<Digest>| {
            window.values().any(|files| files.get(path).map(|e| &e.sha256) == digest.as_ref())
        };
        if observed.iter().any(|(_, path, digest)| !explained(&window, path, digest))
            && let Ok(checkpoint) = self.checkpoint().await
            && checkpoint.size > seq
        {
            match self.release(&checkpoint, seq + 1).await {
                Ok(next) => {
                    window.insert(seq + 1, next);
                }
                Err(error) => report.problems.push(format!("release {}: {error}", seq + 1)),
            }
        }
        report.window = window.keys().copied().collect();
        for (location, path, digest) in observed {
            if explained(&window, &path, &digest) {
                continue;
            }
            let expected: Vec<Digest> =
                window.values().filter_map(|f| f.get(&path).map(|e| e.sha256.clone())).collect();
            report.drift.push(Drift {
                location,
                kind: if digest.is_none() { DriftKind::Missing } else { DriftKind::Changed },
                path,
                expected,
                found: digest,
                window: report.window.clone(),
                detail: format!("{location:?} serves a file no release in the window lists"),
            });
        }
        let released: BTreeSet<&String> = window.values().flat_map(|f| f.keys()).collect();
        for path in &self.config.absent_paths {
            if released.contains(path) {
                continue;
            }
            if let Some(bytes) = self.host.fetch(path).await? {
                report.drift.push(Drift {
                    location: DriftLocation::Host,
                    kind: DriftKind::Unexpected,
                    path: path.clone(),
                    expected: Vec::new(),
                    found: Some(file_digest(&bytes)),
                    window: report.window.clone(),
                    detail: "the host holds a file no release lists".into(),
                });
            }
            if let Ok(url) = self.config.public_origin.join(path)
                && let Ok(fetched) = self.origin_fetch(&url).await
                && fetched.status == 200
            {
                report.drift.push(Drift {
                    location: DriftLocation::Origin,
                    kind: DriftKind::Unexpected,
                    path: path.clone(),
                    expected: Vec::new(),
                    found: Some(file_digest(&fetched.body)),
                    window: report.window.clone(),
                    detail: "the origin serves a path no release lists".into(),
                });
            }
        }
        self.finish(report).await
    }

    async fn finish(&self, mut report: ProbeReport) -> Result<ProbeReport> {
        match self.origin_fetch(&self.config.public_origin).await {
            Ok(home) => {
                report.status.insert(
                    0,
                    StatusProbe { path: "/".into(), status: Some(home.status), error: None },
                );
                let https = self.config.public_origin.scheme() == "https";
                for header in &self.config.headers {
                    if header == "strict-transport-security" && !https {
                        continue;
                    }
                    if home.header(header).is_none() {
                        report.missing_headers.push(header.clone());
                    }
                }
                if let Some(der) = &home.peer_certificate {
                    match CertificateProbe::from_der(
                        der,
                        report.probed_at,
                        self.config.certificate_warning_days,
                    ) {
                        Ok(certificate) => report.certificate = Some(certificate),
                        Err(error) => report.problems.push(error.to_string()),
                    }
                } else if https {
                    report.problems.push("no certificate was read from the origin".into());
                }
            }
            Err(error) => report.status.insert(
                0,
                StatusProbe { path: "/".into(), status: None, error: Some(error.to_string()) },
            ),
        }
        for expectation in &self.config.dns {
            match self.dns.lookup(&expectation.name, expectation.kind).await {
                Ok(values) => {
                    let found: BTreeSet<String> = values.into_iter().collect();
                    report.dns.push(DnsProbe {
                        name: expectation.name.clone(),
                        kind: expectation.kind,
                        matches: found == expectation.values,
                        expected: expectation.values.clone(),
                        found,
                    });
                }
                Err(error) => report.problems.push(error.to_string()),
            }
        }
        if report.drift_detected() {
            tracing::warn!(domain = %self.config.domain, drift = report.drift.len(), "site.drift.detected");
            if let Some((recorder, acting)) = &self.audit {
                let value = serde_json::json!({
                    "domain": self.config.domain,
                    "checkpoint": report.checkpoint,
                    "window": report.window,
                    "drift": report.drift.iter().map(|d| serde_json::json!({ "path": d.path, "location": d.location, "kind": d.kind, "found": d.found })).collect::<Vec<_>>(),
                });
                record(recorder, acting, "site.drift.detected", None, value).await?;
            }
        }
        Ok(report)
    }
}
