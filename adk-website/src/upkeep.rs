//! Upkeep proposals: dependency upgrades for security advisories and certificate renewals
//! (spec 3.5 §5 S10, §7.19 "Guardian … advisories daily", §7.20 "Hosting, domains,
//! certificates, backups — Propose; renewals within budget").
//!
//! The hosting job's daily watch ([`crate::jobs::WATCH_WORKFLOW`]) runs [`UpkeepCheck`]
//! without a model: it reads the edge's `Cargo.lock` against an [`AdvisorySource`] — such as
//! a local clone of the RustSec advisory database ([`AdvisoryDatabase`]) — and the origin's
//! certificate through a [`CertificateSource`], and proposes each upgrade or renewal to the
//! owner as one decision card, filed once. A proposal changes nothing: the owner decides, and
//! the upgrade is a redeploy and the renewal the registrar's or the host's.
//!
//! | Proposal | When | Urgency |
//! |---|---|---|
//! | [`UpkeepProposal::Upgrade`] | A locked package is in a version an advisory names and no patched or unaffected range covers | P2 |
//! | [`UpkeepProposal::RenewCertificate`] | The certificate expires within the warning window (14 days) | P2; P1 within 3 days |

use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_core::{
    AdkError, ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope, ApprovalService,
    ApproverSelector, Clock, Result, SystemClock, ToolConfirmationRequest,
};
use adk_site::migrate::probe::CertificateProbe;
use async_trait::async_trait;
use awp_types::Urgency;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::alarms::{Alarm, AlarmRouter};

/// Days before expiry a certificate renewal is proposed (spec 3.5 §5 S10).
pub const CERTIFICATE_WARNING_DAYS: i64 = 14;

/// A security advisory against a crate (the RustSec advisory format).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Advisory {
    /// Its id, such as `RUSTSEC-2020-0071`.
    pub id: String,
    /// The crate.
    pub package: String,
    /// Its title.
    pub title: String,
    /// Version requirements that carry the fix, such as `>= 0.2.23`.
    #[serde(default)]
    pub patched: Vec<String>,
    /// Version requirements never affected.
    #[serde(default)]
    pub unaffected: Vec<String>,
    /// Where it is described.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Whether it is informational (unmaintained, unsound) rather than a vulnerability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub informational: Option<String>,
}

impl Advisory {
    /// Whether `version` is affected: no patched or unaffected requirement covers it.
    pub fn affects(&self, version: &semver::Version) -> bool {
        let covered = |reqs: &[String]| {
            reqs.iter().any(|req| {
                semver::VersionReq::parse(req).is_ok_and(|parsed| parsed.matches(version))
            })
        };
        !covered(&self.patched) && !covered(&self.unaffected)
    }
}

/// Where advisories come from.
#[async_trait]
pub trait AdvisorySource: Send + Sync {
    /// The advisories against any of `packages`.
    ///
    /// # Errors
    ///
    /// The source's error.
    async fn advisories(&self, packages: &[String]) -> Result<Vec<Advisory>>;
}

/// Advisories held in memory, such as a fixture or a feed already fetched.
#[derive(Debug, Clone, Default)]
pub struct StaticAdvisories(pub Vec<Advisory>);

#[async_trait]
impl AdvisorySource for StaticAdvisories {
    async fn advisories(&self, packages: &[String]) -> Result<Vec<Advisory>> {
        Ok(self.0.iter().filter(|a| packages.contains(&a.package)).cloned().collect())
    }
}

/// A local clone of the RustSec advisory database (`github.com/rustsec/advisory-db`): one
/// Markdown file per advisory under `crates/<crate>/`, its metadata in a TOML block.
#[derive(Debug, Clone)]
pub struct AdvisoryDatabase {
    root: PathBuf,
}

impl AdvisoryDatabase {
    /// The database cloned at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

#[derive(Deserialize)]
struct AdvisoryFile {
    advisory: AdvisoryMeta,
    #[serde(default)]
    versions: AdvisoryVersions,
}

#[derive(Deserialize)]
struct AdvisoryMeta {
    id: String,
    package: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    informational: Option<String>,
    #[serde(default)]
    withdrawn: Option<String>,
}

#[derive(Deserialize, Default)]
struct AdvisoryVersions {
    #[serde(default)]
    patched: Vec<String>,
    #[serde(default)]
    unaffected: Vec<String>,
}

/// Parses one advisory file: a fenced TOML block, then a `# Title` line. `None` for a
/// withdrawn advisory.
///
/// # Errors
///
/// A file with no TOML block or one that does not parse.
pub fn parse_advisory(text: &str) -> Result<Option<Advisory>> {
    let invalid = |why: String| AdkError::config(format!("an advisory file {why}"));
    let start = text.find("```toml").ok_or_else(|| invalid("has no ```toml block".into()))?;
    let body = &text[start + "```toml".len()..];
    let end = body.find("```").ok_or_else(|| invalid("leaves its TOML block open".into()))?;
    let file: AdvisoryFile =
        toml::from_str(&body[..end]).map_err(|e| invalid(format!("does not parse: {e}")))?;
    if file.advisory.withdrawn.is_some() {
        return Ok(None);
    }
    let title = body[end + 3..]
        .lines()
        .find_map(|line| line.trim().strip_prefix("# "))
        .unwrap_or(&file.advisory.id)
        .trim()
        .to_string();
    Ok(Some(Advisory {
        id: file.advisory.id,
        package: file.advisory.package,
        title,
        patched: file.versions.patched,
        unaffected: file.versions.unaffected,
        url: file.advisory.url,
        informational: file.advisory.informational,
    }))
}

#[async_trait]
impl AdvisorySource for AdvisoryDatabase {
    async fn advisories(&self, packages: &[String]) -> Result<Vec<Advisory>> {
        let mut found = Vec::new();
        for package in packages {
            // Crate names are path-safe, and a name that is not is not read.
            if !package.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                continue;
            }
            let dir = self.root.join("crates").join(package);
            let mut entries = match tokio::fs::read_dir(&dir).await {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(AdkError::config(format!("{}: {error}", dir.display())));
                }
            };
            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|e| AdkError::config(format!("{}: {e}", dir.display())))?
            {
                let path = entry.path();
                if path.extension().is_none_or(|ext| ext != "md") {
                    continue;
                }
                let text = tokio::fs::read_to_string(&path)
                    .await
                    .map_err(|e| AdkError::config(format!("{}: {e}", path.display())))?;
                match parse_advisory(&text) {
                    Ok(Some(advisory)) if advisory.package == *package => found.push(advisory),
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(path = %path.display(), error = %error, "an advisory was skipped");
                    }
                }
            }
        }
        found.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(found)
    }
}

/// A package a lockfile pins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedPackage {
    /// The crate.
    pub name: String,
    /// Its version.
    pub version: semver::Version,
}

/// The packages `lockfile` (a `Cargo.lock`) pins.
///
/// # Errors
///
/// A lockfile that does not parse.
pub fn locked_packages(lockfile: &str) -> Result<Vec<LockedPackage>> {
    #[derive(Deserialize)]
    struct Lock {
        #[serde(default)]
        package: Vec<Package>,
    }
    #[derive(Deserialize)]
    struct Package {
        name: String,
        version: String,
    }
    let lock: Lock = toml::from_str(lockfile)
        .map_err(|e| AdkError::config(format!("the lockfile does not parse: {e}")))?;
    Ok(lock
        .package
        .into_iter()
        .filter_map(|p| {
            Some(LockedPackage { version: semver::Version::parse(&p.version).ok()?, name: p.name })
        })
        .collect())
}

/// The origin's certificate, as the probe reads it.
#[async_trait]
pub trait CertificateSource: Send + Sync {
    /// The certificate's expiry, or `None` when the origin serves no certificate.
    ///
    /// # Errors
    ///
    /// The source's error.
    async fn certificate(&self) -> Result<Option<CertificateProbe>>;
}

/// A certificate already read, such as the last probe's.
#[derive(Debug, Clone)]
pub struct StaticCertificate(pub Option<CertificateProbe>);

#[async_trait]
impl CertificateSource for StaticCertificate {
    async fn certificate(&self) -> Result<Option<CertificateProbe>> {
        Ok(self.0.clone())
    }
}

/// The certificate the public origin presents, read through the site's signed HTTP client.
pub struct OriginCertificate {
    http: Arc<adk_site::migrate::http::HttpClient>,
    origin: url::Url,
    warning_days: i64,
}

impl OriginCertificate {
    /// The certificate of `origin`, warning [`CERTIFICATE_WARNING_DAYS`] ahead.
    pub fn new(http: Arc<adk_site::migrate::http::HttpClient>, origin: url::Url) -> Self {
        Self { http, origin, warning_days: CERTIFICATE_WARNING_DAYS }
    }
}

#[async_trait]
impl CertificateSource for OriginCertificate {
    async fn certificate(&self) -> Result<Option<CertificateProbe>> {
        let fetched = self
            .http
            .fetch(&self.origin, &adk_site::migrate::http::FetchOptions::crawler())
            .await
            .map_err(|e| AdkError::config(e.to_string()))?;
        let Some(der) = fetched.peer_certificate else { return Ok(None) };
        CertificateProbe::from_der(&der, Utc::now(), self.warning_days)
            .map(Some)
            .map_err(|e| AdkError::config(e.to_string()))
    }
}

/// An upkeep proposal for the owner (spec 3.5 §7.20).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "proposal", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum UpkeepProposal {
    /// Upgrade a package an advisory names.
    Upgrade {
        /// The advisory.
        advisory: String,
        /// Its title.
        title: String,
        /// The crate.
        package: String,
        /// The locked version.
        version: String,
        /// The versions that carry the fix.
        patched: Vec<String>,
        /// Where it is described.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    /// Renew the site's certificate.
    RenewCertificate {
        /// The site's domain.
        domain: String,
        /// When it expires.
        not_after: DateTime<Utc>,
        /// Whole days left.
        days_left: i64,
    },
}

impl UpkeepProposal {
    /// A stable key: one card per advisory and package, one per certificate expiry.
    pub fn key(&self) -> String {
        match self {
            Self::Upgrade { advisory, package, version, .. } => {
                format!("upgrade-{advisory}-{package}-{version}")
            }
            Self::RenewCertificate { domain, not_after, .. } => {
                format!("renew-{domain}-{}", not_after.format("%Y%m%d"))
            }
        }
    }

    /// The proposal in a sentence for the owner.
    pub fn summary(&self) -> String {
        match self {
            Self::Upgrade { advisory, title, package, version, patched, .. } => {
                let fixed = if patched.is_empty() {
                    "no fixed version yet; replace it or accept the risk".to_string()
                } else {
                    format!("upgrade to {}", patched.join(" or "))
                };
                format!(
                    "{advisory} affects the edge's {package} {version} ({title}): {fixed}, then redeploy"
                )
            }
            Self::RenewCertificate { domain, not_after, days_left } => format!(
                "The certificate of {domain} expires in {days_left} days ({}): renew it",
                not_after.format("%-d %B")
            ),
        }
    }

    /// How soon the owner hears of it.
    pub fn urgency(&self) -> Urgency {
        match self {
            Self::RenewCertificate { days_left, .. } if *days_left <= 3 => Urgency::P1,
            _ => Urgency::P2,
        }
    }
}

/// The proposals for `packages` under `advisories`: every vulnerability that affects a
/// locked version, informational advisories left out.
pub fn upgrades(packages: &[LockedPackage], advisories: &[Advisory]) -> Vec<UpkeepProposal> {
    let mut proposals = Vec::new();
    for advisory in advisories.iter().filter(|a| a.informational.is_none()) {
        for package in packages.iter().filter(|p| p.name == advisory.package) {
            if advisory.affects(&package.version) {
                proposals.push(UpkeepProposal::Upgrade {
                    advisory: advisory.id.clone(),
                    title: advisory.title.clone(),
                    package: package.name.clone(),
                    version: package.version.to_string(),
                    patched: advisory.patched.clone(),
                    url: advisory.url.clone(),
                });
            }
        }
    }
    proposals
}

/// What one upkeep check found and filed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpkeepReport {
    /// Every proposal the check found, filed now or earlier.
    pub proposals: Vec<UpkeepProposal>,
    /// The decision cards filed now.
    pub filed: Vec<String>,
    /// What the check could not read.
    pub problems: Vec<String>,
}

/// The hosting job's daily upkeep check (spec 3.5 §7.20): advisories against the edge's
/// lockfile and the certificate's expiry, each found proposal filed once as a decision card
/// to the owner, with an alarm by its urgency.
pub struct UpkeepCheck {
    site: Uuid,
    domain: String,
    app: String,
    owner: String,
    approvals: Arc<dyn ApprovalService>,
    alarms: Option<Arc<AlarmRouter>>,
    lockfile: Option<PathBuf>,
    advisories: Option<Arc<dyn AdvisorySource>>,
    certificate: Option<Arc<dyn CertificateSource>>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for UpkeepCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpkeepCheck").field("domain", &self.domain).finish_non_exhaustive()
    }
}

impl UpkeepCheck {
    /// A check for `site` at `domain` of app `app`, filing cards for `owner` in `approvals`.
    pub fn new(
        site: Uuid,
        domain: impl Into<String>,
        app: impl Into<String>,
        owner: impl Into<String>,
        approvals: Arc<dyn ApprovalService>,
    ) -> Self {
        Self {
            site,
            domain: domain.into(),
            app: app.into(),
            owner: owner.into(),
            approvals,
            alarms: None,
            lockfile: None,
            advisories: None,
            certificate: None,
            clock: Arc::new(SystemClock),
        }
    }

    /// Reads `lockfile` against `source` (spec 3.5 §7.19 "advisories daily").
    #[must_use]
    pub fn with_advisories(
        mut self,
        lockfile: impl AsRef<Path>,
        source: Arc<dyn AdvisorySource>,
    ) -> Self {
        self.lockfile = Some(lockfile.as_ref().to_path_buf());
        self.advisories = Some(source);
        self
    }

    /// Reads the certificate's expiry from `source`.
    #[must_use]
    pub fn with_certificate(mut self, source: Arc<dyn CertificateSource>) -> Self {
        self.certificate = Some(source);
        self
    }

    /// Raises each new proposal's alarm on `router`.
    #[must_use]
    pub fn with_alarms(mut self, router: Arc<AlarmRouter>) -> Self {
        self.alarms = Some(router);
        self
    }

    /// The clock.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Finds the proposals and files the new ones (spec 3.5 §7.20).
    ///
    /// # Errors
    ///
    /// The approval service's error; what cannot be read is reported, not raised.
    #[tracing::instrument(skip_all, name = "website.job.hosting", fields(site.domain = %self.domain))]
    pub async fn run(&self) -> Result<UpkeepReport> {
        let mut proposals = Vec::new();
        let mut problems = Vec::new();
        if let (Some(path), Some(source)) = (&self.lockfile, &self.advisories) {
            let read = async {
                let text = tokio::fs::read_to_string(path)
                    .await
                    .map_err(|e| AdkError::config(format!("{}: {e}", path.display())))?;
                let packages = locked_packages(&text)?;
                let names: Vec<String> = packages.iter().map(|p| p.name.clone()).collect();
                let advisories = source.advisories(&names).await?;
                Ok::<_, AdkError>(upgrades(&packages, &advisories))
            };
            match read.await {
                Ok(found) => proposals.extend(found),
                Err(error) => problems.push(format!("advisories: {error}")),
            }
        }
        if let Some(source) = &self.certificate {
            match source.certificate().await {
                Ok(Some(certificate)) if certificate.expiring => {
                    proposals.push(UpkeepProposal::RenewCertificate {
                        domain: self.domain.clone(),
                        not_after: certificate.not_after,
                        days_left: certificate.days_left,
                    });
                }
                Ok(_) => {}
                Err(error) => problems.push(format!("certificate: {error}")),
            }
        }
        let mut filed = Vec::new();
        for proposal in &proposals {
            let id = format!("site-upkeep-{}-{}", self.site, proposal.key());
            if self.approvals.get(&id).await?.is_some() {
                continue;
            }
            let call = ToolConfirmationRequest::new("upkeep_proposal", json!(proposal));
            let scope = ApprovalScope::new(self.app.clone(), self.owner.clone(), "site-upkeep");
            let policy = ApprovalPolicy::new([ApproverSelector::human(self.owner.clone())]);
            let request = ApprovalRequest::new(id.clone(), scope, &call, policy)
                .with_summary(proposal.summary())
                .with_risk(ApprovalRisk::Medium);
            self.approvals.submit(request).await?;
            if let Some(alarms) = &self.alarms {
                alarms
                    .raise(Alarm::new(
                        self.site,
                        proposal.urgency(),
                        "upkeep_proposal",
                        proposal.summary(),
                        self.clock.now(),
                    ))
                    .await;
            }
            tracing::info!(website.site = %self.site, proposal = %proposal.key(), "website.upkeep.proposed");
            filed.push(id);
        }
        Ok(UpkeepReport { proposals, filed, problems })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADVISORY: &str = r#"```toml
[advisory]
id = "RUSTSEC-2020-0071"
package = "time"
date = "2020-11-18"
url = "https://github.com/time-rs/time/issues/293"

[versions]
patched = [">= 0.2.23"]
unaffected = ["= 0.2.0", "= 0.2.1", "= 0.2.2"]
```

# Potential segfault in the time crate

Unix-like operating systems may segfault.
"#;

    #[test]
    fn an_advisory_parses_and_names_the_versions_it_affects() {
        let advisory = parse_advisory(ADVISORY).unwrap().unwrap();
        assert_eq!(advisory.id, "RUSTSEC-2020-0071");
        assert_eq!(advisory.title, "Potential segfault in the time crate");
        let affected = |v: &str| advisory.affects(&semver::Version::parse(v).unwrap());
        assert!(affected("0.1.45"));
        assert!(!affected("0.2.1"), "unaffected");
        assert!(!affected("0.3.36"), "patched");
        let withdrawn = ADVISORY.replace("[versions]", "withdrawn = \"2021-01-01\"\n[versions]");
        assert!(parse_advisory(&withdrawn).unwrap().is_none());
    }

    #[test]
    fn a_locked_vulnerable_version_becomes_one_upgrade_proposal() {
        let lock = r#"
version = 4

[[package]]
name = "time"
version = "0.1.45"

[[package]]
name = "serde"
version = "1.0.228"
"#;
        let packages = locked_packages(lock).unwrap();
        let advisory = parse_advisory(ADVISORY).unwrap().unwrap();
        let proposals = upgrades(&packages, std::slice::from_ref(&advisory));
        assert_eq!(proposals.len(), 1);
        assert!(proposals[0].summary().contains("upgrade to >= 0.2.23"), "{proposals:?}");
        let mut note = advisory;
        note.informational = Some("unmaintained".into());
        assert!(upgrades(&packages, &[note]).is_empty(), "informational advisories wait");
    }
}
