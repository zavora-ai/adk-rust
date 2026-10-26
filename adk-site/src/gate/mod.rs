//! The launch gate: fixed checks outside the agent's write scope (spec 3.5 §7.8.1, §7.8.2
//! step 2, §7.8.3 "Tiered checks").
//!
//! | Check | Pass condition | Fast | Full |
//! |-------|----------------|------|------|
//! | Server code | Zero server-executable files or server-side include directives (A-I1); always fatal | written files | every file |
//! | Claims | Zero published claims from facts that are not `Known` (A-I18), through a [`ClaimCheck`]; review quotes only with the reviewer's public name and the owner's approval ([`QuotableReview`]) | changed items | every item |
//! | Links | Zero broken internal links | written pages | every page |
//! | Accessibility | Zero serious WCAG 2.2 AA violations; image alt text confirmed; theme contrast | written pages | every page, contrast |
//! | Performance | Critical path within the budget (LCP recorded as not measured) | — | every page |
//! | Legal | Privacy, consent and terms pages for every jurisdiction | — | yes |
//! | Machine views | Every page has its twins, linked and valid | written pages | every page |
//! | Quarantine | Zero quarantined items or files in the output | yes | yes |
//! | Required languages | Every item in every required language | changed items | every item |
//! | Red lines | No item states a charter red line | changed items | every item |
//! | Structured data | Fields each kind needs; JSON-LD parses | changed items | every item |
//!
//! Further checks plug in as [`GateExtension`]s: migration parity (`migrate` feature),
//! [`ThirdPartyResources`] for kinds that ship no third-party scripts or trackers,
//! [`ReleaseDelegation`] for a served site whose release key the owner's root delegates, and
//! `PreviewVerification` (`verify` feature), which runs `awp verify` against the preview.

mod extensions;
mod html;
mod links;
mod reviews;
#[cfg(feature = "verify")]
mod verify;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use awp_types::{
    ContentId, ContentItem, ContentKind, ContentState, FactRef, MachineView, RedLine, RedLineKind,
};
use scraper::Html;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::content::SiteProject;
use crate::error::{Result, SiteError};
use crate::generator::BuildReport;
use crate::scan::scan_file;

pub use extensions::{ReleaseDelegation, ThirdPartyResources};
pub use html::{A11yViolation, Severity};
pub use reviews::{QuotableReview, review_findings};
#[cfg(feature = "verify")]
pub use verify::PreviewVerification;

/// A launch-gate check (spec 3.5 §7.8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum GateCheck {
    /// A-I1.
    ServerCode,
    /// A-I18.
    Claims,
    /// Internal links.
    Links,
    /// WCAG 2.2 AA automated checks.
    Accessibility,
    /// The performance budget.
    Performance,
    /// Legal pages per jurisdiction.
    Legal,
    /// Twins of every page.
    MachineViews,
    /// Migration parity (a [`GateExtension`]).
    MigrationParity,
    /// Quarantined items.
    Quarantine,
    /// Required languages.
    RequiredLanguages,
    /// Charter red lines.
    RedLines,
    /// Structured data per content kind.
    StructuredData,
    /// Resources loaded from other origins ([`ThirdPartyResources`], spec 3.5 §7.16.1).
    ThirdPartyResources,
    /// Release signing and its delegation by the owner's root ([`ReleaseDelegation`], D-11).
    Signing,
    /// `awp verify` against the preview (`PreviewVerification`, feature `verify`; spec 3.5
    /// §7.8.1 "Machine views").
    AwpVerify,
}

/// Which tier runs (spec 3.5 §7.8.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateMode {
    /// Per change: the changed items and the files the build wrote.
    Fast,
    /// Before launch and on schedule: everything.
    Full,
}

/// How a check ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// No blocking finding.
    Passed,
    /// At least one blocking finding.
    Failed,
    /// The check could not measure; see the note.
    NotMeasured,
    /// The tier does not run the check.
    Skipped,
}

/// One finding of a check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    /// The rule, such as `image-alt` or `broken-link`.
    pub rule: String,
    /// How serious it is.
    pub severity: Severity,
    /// Whether it fails the gate.
    pub blocking: bool,
    /// The file, when the finding is a file's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The item, when the finding is an item's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ContentId>,
    /// What is wrong and what to do.
    pub message: String,
}

impl Finding {
    fn blocking(
        rule: &str,
        path: Option<String>,
        item: Option<ContentId>,
        message: String,
    ) -> Self {
        Self { rule: rule.into(), severity: Severity::Serious, blocking: true, path, item, message }
    }
}

/// The result of one check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckResult {
    /// The check.
    pub check: GateCheck,
    /// How it ended.
    pub status: CheckStatus,
    /// Its findings, blocking or not.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<Finding>,
    /// What it did not measure, and why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl CheckResult {
    fn from_findings(check: GateCheck, findings: Vec<Finding>) -> Self {
        let status = if findings.iter().any(|f| f.blocking) {
            CheckStatus::Failed
        } else {
            CheckStatus::Passed
        };
        Self { check, status, findings, note: None }
    }

    fn skipped(check: GateCheck) -> Self {
        Self {
            check,
            status: CheckStatus::Skipped,
            findings: Vec::new(),
            note: Some("not in the fast tier".into()),
        }
    }

    fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }
}

/// The gate's verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GateReport {
    /// The tier that ran.
    pub mode: GateMode,
    /// Every check, in table order.
    pub checks: Vec<CheckResult>,
}

impl GateReport {
    /// Whether every check passed, measured nothing it could not, or was skipped.
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|c| c.status != CheckStatus::Failed)
    }

    /// The result of `check`.
    pub fn check(&self, check: GateCheck) -> Option<&CheckResult> {
        self.checks.iter().find(|c| c.check == check)
    }

    /// Every blocking finding.
    pub fn blocking(&self) -> impl Iterator<Item = (GateCheck, &Finding)> {
        self.checks
            .iter()
            .flat_map(|c| c.findings.iter().filter(|f| f.blocking).map(move |f| (c.check, f)))
    }

    /// Whether the release must be refused whole: a server-code finding (A-I1).
    pub fn refuses_release(&self) -> bool {
        self.check(GateCheck::ServerCode).is_some_and(|c| c.status == CheckStatus::Failed)
    }

    /// The items a fast-tier failure holds; the rest of a batch may proceed unless the
    /// release is refused (spec 3.5 §7.8.3).
    pub fn held_items(&self) -> BTreeSet<ContentId> {
        self.blocking().filter_map(|(_, f)| f.item.clone()).collect()
    }

    /// A one-line summary of the blocking findings.
    pub fn summary(&self) -> String {
        let failed: Vec<String> = self
            .checks
            .iter()
            .filter(|c| c.status == CheckStatus::Failed)
            .map(|c| format!("{:?}: {}", c.check, c.findings.iter().filter(|f| f.blocking).count()))
            .collect();
        if failed.is_empty() { "passed".into() } else { failed.join(", ") }
    }
}

/// A claim that does not rest on a `Known` fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimFinding {
    /// The claim or field.
    pub claim: String,
    /// The fact it cites, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fact: Option<FactRef>,
    /// Why it fails.
    pub reason: String,
}

/// Decides whether an item's claims rest on `Known` facts (spec 3.5 A-I18). The caller
/// implements it over its facts store; [`FactTable`] is a static one.
#[async_trait]
pub trait ClaimCheck: Send + Sync {
    /// The claims in `item` that do not rest on `Known` facts; empty when every one does.
    ///
    /// # Errors
    ///
    /// Returns an error when the facts cannot be read.
    async fn unsupported_claims(&self, item: &ContentItem) -> Result<Vec<ClaimFinding>>;
}

/// The standing of a fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactStatus {
    /// Confirmed by the owner.
    Known,
    /// Found or inferred, not confirmed.
    Assumed,
}

/// A [`ClaimCheck`] over a fixed table: every fact an item cites must be `Known`.
///
/// # Example
///
/// ```rust
/// use adk_site::gate::{FactStatus, FactTable};
/// use awp_types::FactRef;
///
/// let facts = FactTable::new([(FactRef("founded-2019".into()), FactStatus::Known)]);
/// # let _ = facts;
/// ```
#[derive(Debug, Clone, Default)]
pub struct FactTable {
    facts: BTreeMap<FactRef, FactStatus>,
}

impl FactTable {
    /// A table of facts.
    pub fn new(facts: impl IntoIterator<Item = (FactRef, FactStatus)>) -> Self {
        Self { facts: facts.into_iter().collect() }
    }
}

#[async_trait]
impl ClaimCheck for FactTable {
    async fn unsupported_claims(&self, item: &ContentItem) -> Result<Vec<ClaimFinding>> {
        Ok(item
            .sources
            .iter()
            .filter_map(|fact| match self.facts.get(fact) {
                Some(FactStatus::Known) => None,
                Some(FactStatus::Assumed) => Some(ClaimFinding {
                    claim: fact.0.clone(),
                    fact: Some(fact.clone()),
                    reason: "the fact is Assumed; the owner has not confirmed it".into(),
                }),
                None => Some(ClaimFinding {
                    claim: fact.0.clone(),
                    fact: Some(fact.clone()),
                    reason: "the fact is not in the site's facts".into(),
                }),
            })
            .collect())
    }
}

/// Items and files the sanitiser quarantined; none may ship without a per-item decision.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Quarantine {
    /// Quarantined items.
    pub items: BTreeSet<ContentId>,
    /// Quarantined output paths.
    pub paths: BTreeSet<String>,
}

/// What the gate checks.
#[derive(Debug, Clone, Copy)]
pub struct GateInput<'a> {
    /// The project that was built.
    pub project: &'a SiteProject,
    /// The build output directory.
    pub out: &'a Path,
    /// The build's report.
    pub report: &'a BuildReport,
    /// The changed items, for the fast tier; `None` checks every item.
    pub changed: Option<&'a BTreeSet<ContentId>>,
    /// The quarantine.
    pub quarantine: &'a Quarantine,
}

/// A check the gate runs beside its own: migration parity, `awp verify` against a preview.
#[async_trait]
pub trait GateExtension: Send + Sync {
    /// The check it reports as.
    fn check(&self) -> GateCheck;

    /// Whether it runs in `mode`.
    fn runs_in(&self, mode: GateMode) -> bool {
        mode == GateMode::Full
    }

    /// Runs the check.
    ///
    /// # Errors
    ///
    /// Returns an error when the check cannot run; the gate reports it as failed.
    async fn run(&self, input: &GateInput<'_>) -> Result<CheckResult>;
}

/// The launch gate (spec 3.5 §7.8.1).
///
/// # Example
///
/// ```rust,no_run
/// use std::collections::BTreeSet;
/// use std::sync::Arc;
/// use adk_site::gate::{FactTable, GateInput, GateMode, LaunchGate, Quarantine};
/// # use adk_site::{content::SiteProject, generator::BuildReport};
///
/// # async fn demo(project: &SiteProject, report: &BuildReport, out: &std::path::Path) -> adk_site::Result<()> {
/// let gate = LaunchGate::new().with_claim_check(Arc::new(FactTable::default()));
/// let quarantine = Quarantine::default();
/// let verdict = gate
///     .run(GateMode::Full, &GateInput { project, out, report, changed: None, quarantine: &quarantine })
///     .await?;
/// assert!(verdict.passed(), "{}", verdict.summary());
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Default)]
pub struct LaunchGate {
    claims: Option<Arc<dyn ClaimCheck>>,
    red_lines: Vec<RedLine>,
    reviews: Vec<QuotableReview>,
    extensions: Vec<Arc<dyn GateExtension>>,
}

impl std::fmt::Debug for LaunchGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchGate")
            .field("claims", &self.claims.is_some())
            .field("red_lines", &self.red_lines)
            .field("reviews", &self.reviews.len())
            .field("extensions", &self.extensions.len())
            .finish()
    }
}

const REQUIRED_LEGAL: &[&str] = &["privacy", "consent", "terms"];

fn is_html(path: &str) -> bool {
    path.ends_with(".html") || path.ends_with(".htm")
}

impl LaunchGate {
    /// A gate with no claim check, no red lines and no extensions.
    pub fn new() -> Self {
        Self::default()
    }

    /// Checks claims with `check`.
    pub fn with_claim_check(mut self, check: Arc<dyn ClaimCheck>) -> Self {
        self.claims = Some(check);
        self
    }

    /// Refuses items that state one of the charter's red lines.
    pub fn with_red_lines(mut self, red_lines: Vec<RedLine>) -> Self {
        self.red_lines = red_lines;
        self
    }

    /// The reviews discovery found, with their attribution and the owner's approvals: an
    /// item quoting one fails the claims check unless it names the reviewer and the owner
    /// approved quoting it (spec 3.5 S2).
    pub fn with_reviews(mut self, reviews: Vec<QuotableReview>) -> Self {
        self.reviews = reviews;
        self
    }

    /// Adds a check.
    pub fn with_extension(mut self, extension: Arc<dyn GateExtension>) -> Self {
        self.extensions.push(extension);
        self
    }

    /// Runs the tier `mode` over a build.
    ///
    /// # Errors
    ///
    /// [`SiteError::Io`] when the output cannot be read; errors from the claim check.
    pub async fn run(&self, mode: GateMode, input: &GateInput<'_>) -> Result<GateReport> {
        let changed = match mode {
            GateMode::Fast => input.changed,
            GateMode::Full => None,
        };
        let on_disk = {
            let out = input.out.to_path_buf();
            tokio::task::spawn_blocking(move || crate::generator::list_files(&out))
                .await
                .map_err(|e| SiteError::Pipeline(e.to_string()))??
        };
        let scope: BTreeSet<String> = match mode {
            GateMode::Fast => input.report.written.intersection(&on_disk).cloned().collect(),
            GateMode::Full => on_disk.clone(),
        };
        let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for path in &scope {
            let full = input.out.join(path);
            let bytes = tokio::fs::read(&full).await.map_err(|e| SiteError::io(full, e))?;
            files.insert(path.clone(), bytes);
        }
        let live: Vec<&ContentItem> = {
            let shown: BTreeSet<&ContentId> =
                input.report.pages.iter().filter_map(|p| p.item.as_ref()).collect();
            input.project.items.iter().filter(|item| shown.contains(&item.id)).collect()
        };
        let items: Vec<&ContentItem> = live
            .iter()
            .copied()
            .filter(|item| changed.is_none_or(|c| c.contains(&item.id)))
            .collect();

        let mut checks = vec![
            server_code(&files),
            self.claims(&items).await?,
            check_links(input, &files, &on_disk),
            accessibility(mode, input, &files, &items),
            match mode {
                GateMode::Full => performance(input, &files, &on_disk),
                GateMode::Fast => CheckResult::skipped(GateCheck::Performance),
            },
            match mode {
                GateMode::Full => legal(input, &live),
                GateMode::Fast => CheckResult::skipped(GateCheck::Legal),
            },
            machine_views(mode, input, &files, &on_disk),
            quarantine(input, &on_disk),
            required_languages(input, &items),
            self.red_lines(&items),
            structured_data(&items, &files),
        ];
        for extension in &self.extensions {
            if !extension.runs_in(mode) {
                continue;
            }
            let result = match extension.run(input).await {
                Ok(result) => result,
                Err(error) => CheckResult::from_findings(
                    extension.check(),
                    vec![Finding::blocking("extension-error", None, None, error.to_string())],
                ),
            };
            checks.push(result);
        }
        Ok(GateReport { mode, checks })
    }

    async fn claims(&self, items: &[&ContentItem]) -> Result<CheckResult> {
        let quotes: Vec<Finding> =
            items.iter().flat_map(|item| reviews::review_findings(item, &self.reviews)).collect();
        let Some(check) = &self.claims else {
            let citing: Vec<&&ContentItem> =
                items.iter().filter(|i| !i.sources.is_empty()).collect();
            if citing.is_empty() && quotes.is_empty() {
                return Ok(CheckResult {
                    check: GateCheck::Claims,
                    status: CheckStatus::NotMeasured,
                    findings: Vec::new(),
                    note: Some("no claim check is configured and no item cites a fact".into()),
                });
            }
            let mut findings: Vec<Finding> = citing
                .iter()
                .map(|item| {
                    Finding::blocking(
                        "claims-unverified",
                        None,
                        Some(item.id.clone()),
                        "the item cites facts and no claim check is configured; configure one so claims are checked against Known facts (A-I18)".into(),
                    )
                })
                .collect();
            findings.extend(quotes);
            return Ok(CheckResult::from_findings(GateCheck::Claims, findings));
        };
        let mut findings = quotes;
        for item in items {
            for claim in check.unsupported_claims(item).await? {
                findings.push(Finding::blocking(
                    "claim-not-known",
                    None,
                    Some(item.id.clone()),
                    format!(
                        "claim '{}' is not from a Known fact: {} (A-I18)",
                        claim.claim, claim.reason
                    ),
                ));
            }
        }
        Ok(CheckResult::from_findings(GateCheck::Claims, findings))
    }

    fn red_lines(&self, items: &[&ContentItem]) -> CheckResult {
        let mut findings = Vec::new();
        for item in items {
            let mut text = item.body.as_ref().map(|b| b.0.to_lowercase()).unwrap_or_default();
            for value in item.fields.values() {
                collect_text(value, &mut text);
            }
            for line in &self.red_lines {
                if !matches!(line.kind, RedLineKind::Claim | RedLineKind::Topic) {
                    continue;
                }
                let phrase = line.text.trim().to_lowercase();
                if !phrase.is_empty() && text.contains(&phrase) {
                    findings.push(Finding::blocking(
                        "red-line",
                        None,
                        Some(item.id.clone()),
                        format!("the item states the red line '{}'", line.text),
                    ));
                }
            }
        }
        CheckResult::from_findings(GateCheck::RedLines, findings)
    }
}

fn collect_text(value: &Value, out: &mut String) {
    match value {
        Value::String(s) => {
            out.push('\n');
            out.push_str(&s.to_lowercase());
        }
        Value::Array(values) => values.iter().for_each(|v| collect_text(v, out)),
        Value::Object(map) => map.values().for_each(|v| collect_text(v, out)),
        _ => {}
    }
}

fn server_code(files: &BTreeMap<String, Vec<u8>>) -> CheckResult {
    let findings = files
        .iter()
        .flat_map(|(path, bytes)| scan_file(path, bytes))
        .map(|f| Finding {
            rule: "server-code".into(),
            severity: Severity::Critical,
            blocking: true,
            message: format!("{:?} (A-I1: no flag overrides this)", f.kind),
            path: Some(f.path),
            item: None,
        })
        .collect();
    CheckResult::from_findings(GateCheck::ServerCode, findings)
}

fn item_of_path<'a>(report: &'a BuildReport, path: &str) -> Option<&'a ContentId> {
    report
        .pages
        .iter()
        .find(|p| {
            p.html == path
                || p.markdown.as_deref() == Some(path)
                || p.machine_view.as_deref() == Some(path)
        })
        .and_then(|p| p.item.as_ref())
}

fn check_links(
    input: &GateInput<'_>,
    files: &BTreeMap<String, Vec<u8>>,
    on_disk: &BTreeSet<String>,
) -> CheckResult {
    let redirects: BTreeSet<String> =
        input.report.redirects.iter().filter(|r| r.status != 410).map(|r| r.from.clone()).collect();
    let domain = &input.project.config.domain;
    let mut findings = Vec::new();
    for (path, bytes) in files.iter().filter(|(p, _)| is_html(p)) {
        let document = Html::parse_document(&String::from_utf8_lossy(bytes));
        let mut seen = BTreeSet::new();
        for link in html::links(&document) {
            if let links::LinkTarget::Internal(target) = links::resolve(path, &link.url, domain)
                && !links::exists(&target, on_disk, &redirects)
                && seen.insert(target.clone())
            {
                findings.push(Finding::blocking(
                    "broken-link",
                    Some(path.clone()),
                    item_of_path(input.report, path).cloned(),
                    format!(
                        "<{}> links to '{}', which the release does not contain",
                        link.tag, link.url
                    ),
                ));
            }
        }
    }
    CheckResult::from_findings(GateCheck::Links, findings)
}

fn accessibility(
    mode: GateMode,
    input: &GateInput<'_>,
    files: &BTreeMap<String, Vec<u8>>,
    items: &[&ContentItem],
) -> CheckResult {
    let mut findings = Vec::new();
    for (path, bytes) in files.iter().filter(|(p, _)| is_html(p)) {
        let document = Html::parse_document(&String::from_utf8_lossy(bytes));
        for violation in html::accessibility(&document) {
            findings.push(Finding {
                rule: violation.rule.into(),
                severity: violation.severity,
                blocking: violation.severity.fails_gate(),
                path: Some(path.clone()),
                item: item_of_path(input.report, path).cloned(),
                message: format!(
                    "WCAG {} {}: {}",
                    violation.wcag, violation.element, violation.help
                ),
            });
        }
    }
    for item in items {
        for media in item.media.iter().filter(|m| m.content_type.starts_with("image/")) {
            if !media.alt_confirmed || media.alt.is_none() {
                findings.push(Finding {
                    rule: "media-alt-confirmed".into(),
                    severity: Severity::Critical,
                    blocking: true,
                    path: Some(media.path.clone()),
                    item: Some(item.id.clone()),
                    message: "WCAG 1.1.1: the image's alt text is not confirmed by a person".into(),
                });
            }
        }
    }
    let mut result = CheckResult::from_findings(GateCheck::Accessibility, findings);
    if mode == GateMode::Full {
        if input.report.theme == input.project.theme.name {
            for pair in input.project.theme.tokens.contrast_pairs() {
                if !pair.passes() {
                    result.findings.push(Finding::blocking(
                        "color-contrast",
                        None,
                        None,
                        format!(
                            "WCAG 1.4.3: theme pair {} ({} on {}) is {:.2}:1; it needs {}:1",
                            pair.name, pair.foreground, pair.background, pair.ratio, pair.required
                        ),
                    ));
                    result.status = CheckStatus::Failed;
                }
            }
        } else {
            result.note =
                Some("the theme's tokens are not the project's; contrast not measured".into());
        }
    }
    result
}

fn performance(
    input: &GateInput<'_>,
    files: &BTreeMap<String, Vec<u8>>,
    on_disk: &BTreeSet<String>,
) -> CheckResult {
    let budget = input.project.config.performance.critical_path_bytes;
    let domain = &input.project.config.domain;
    let size_of = |path: &str| -> u64 {
        files.get(path).map(|b| b.len() as u64).unwrap_or_else(|| {
            std::fs::metadata(input.out.join(path)).map(|m| m.len()).unwrap_or(0)
        })
    };
    let mut findings = Vec::new();
    let mut external = 0;
    for (path, bytes) in files.iter().filter(|(p, _)| is_html(p)) {
        let document = Html::parse_document(&String::from_utf8_lossy(bytes));
        let mut total = bytes.len() as u64;
        for resource in html::critical_resources(&document) {
            match links::resolve(path, &resource, domain) {
                links::LinkTarget::Internal(target) => {
                    let target = if target.ends_with('/') || target.is_empty() {
                        format!("{target}index.html")
                    } else {
                        target
                    };
                    if on_disk.contains(&target) {
                        total += size_of(&target);
                    }
                }
                links::LinkTarget::External => external += 1,
                links::LinkTarget::Fragment => {}
            }
        }
        if total > budget {
            findings.push(Finding::blocking(
                "critical-path-budget",
                Some(path.clone()),
                item_of_path(input.report, path).cloned(),
                format!("the critical path is {total} bytes; the budget is {budget}"),
            ));
        }
    }
    let mut note = format!(
        "LCP not measured: the built-in gate renders nothing; the {} ms budget needs a browser on the reference profile",
        input.project.config.performance.lcp_ms
    );
    if external > 0 {
        note.push_str(&format!("; {external} external render-blocking resource(s) not counted"));
    }
    CheckResult::from_findings(GateCheck::Performance, findings).note(note)
}

fn legal(input: &GateInput<'_>, live: &[&ContentItem]) -> CheckResult {
    let jurisdictions: Vec<String> = if input.project.config.jurisdictions.is_empty() {
        vec!["*".into()]
    } else {
        input.project.config.jurisdictions.clone()
    };
    let mut findings = Vec::new();
    for jurisdiction in &jurisdictions {
        for kind in REQUIRED_LEGAL {
            let present = live.iter().any(|item| {
                let legal = item.fields.get("legal").and_then(Value::as_str);
                let covers =
                    item.fields.get("jurisdictions").and_then(Value::as_array).is_none_or(|list| {
                        jurisdiction == "*"
                            || list.iter().any(|j| j.as_str() == Some(jurisdiction.as_str()))
                    });
                legal == Some(kind) && covers
            });
            if !present {
                findings.push(Finding::blocking(
                    "legal-page-missing",
                    None,
                    None,
                    format!("no published {kind} page covers jurisdiction '{jurisdiction}'; add a page with `legal: {kind}`"),
                ));
            }
        }
    }
    CheckResult::from_findings(GateCheck::Legal, findings)
}

fn machine_views(
    mode: GateMode,
    input: &GateInput<'_>,
    files: &BTreeMap<String, Vec<u8>>,
    on_disk: &BTreeSet<String>,
) -> CheckResult {
    let mut findings = Vec::new();
    for page in &input.report.pages {
        if !files.contains_key(&page.html) {
            continue;
        }
        let (Some(md), Some(view)) = (&page.markdown, &page.machine_view) else { continue };
        let missing = |what: &str, file: &str| {
            Finding::blocking(
                "twin-missing",
                Some(page.html.clone()),
                page.item.clone(),
                format!("the page has no {what} at '{file}'"),
            )
        };
        if !on_disk.contains(md) {
            findings.push(missing("Markdown twin", md));
        }
        if !on_disk.contains(view) {
            findings.push(missing("machine view", view));
        } else {
            let bytes = files
                .get(view)
                .cloned()
                .or_else(|| std::fs::read(input.out.join(view)).ok())
                .unwrap_or_default();
            match serde_json::from_slice::<MachineView>(&bytes) {
                Ok(parsed) if parsed.path == page.path => {}
                Ok(parsed) => findings.push(Finding::blocking(
                    "machine-view-invalid",
                    Some(view.clone()),
                    page.item.clone(),
                    format!("the machine view names path '{}', not '{}'", parsed.path, page.path),
                )),
                Err(e) => findings.push(Finding::blocking(
                    "machine-view-invalid",
                    Some(view.clone()),
                    page.item.clone(),
                    format!("the machine view is not an AWP machine view: {e}"),
                )),
            }
        }
        let document = Html::parse_document(&String::from_utf8_lossy(&files[&page.html]));
        let alternates = html::alternates(&document);
        for (kind, file) in [("text/markdown", md), ("application/awp+json", view)] {
            let expected = format!("/{file}");
            if !alternates
                .iter()
                .any(|(t, href)| t == kind && (href == &expected || href.ends_with(&expected)))
            {
                findings.push(Finding::blocking(
                    "twin-not-linked",
                    Some(page.html.clone()),
                    page.item.clone(),
                    format!("the page does not link its {kind} twin with <link rel=\"alternate\">"),
                ));
            }
        }
    }
    let mut result = CheckResult::from_findings(GateCheck::MachineViews, findings);
    if mode == GateMode::Full {
        let contract = ".well-known/awp.json";
        if on_disk.contains(contract) {
            let bytes = std::fs::read(input.out.join(contract)).unwrap_or_default();
            if let Err(e) = serde_json::from_slice::<awp_types::SiteContract>(&bytes) {
                result.findings.push(Finding::blocking(
                    "contract-invalid",
                    Some(contract.into()),
                    None,
                    e.to_string(),
                ));
                result.status = CheckStatus::Failed;
            }
        }
        result.note = Some("`awp verify` against the preview runs as a gate extension".into());
    }
    result
}

fn quarantine(input: &GateInput<'_>, on_disk: &BTreeSet<String>) -> CheckResult {
    let mut findings = Vec::new();
    for page in &input.report.pages {
        if let Some(item) = &page.item
            && input.quarantine.items.contains(item)
        {
            findings.push(Finding::blocking(
                "quarantined-item",
                Some(page.html.clone()),
                Some(item.clone()),
                "the item is quarantined; it publishes only after a per-item decision".into(),
            ));
        }
    }
    for path in input.quarantine.paths.intersection(on_disk) {
        findings.push(Finding::blocking(
            "quarantined-file",
            Some(path.clone()),
            None,
            "the file is quarantined; it publishes only after a per-item decision".into(),
        ));
    }
    CheckResult::from_findings(GateCheck::Quarantine, findings)
}

fn required_languages(input: &GateInput<'_>, items: &[&ContentItem]) -> CheckResult {
    let policy = &input.project.config.translation;
    let mut findings = Vec::new();
    if !policy.required.is_empty() {
        for item in items {
            let group: Vec<&ContentItem> = input
                .project
                .items
                .iter()
                .filter(|other| other.kind == item.kind && other.slug == item.slug)
                .collect();
            let missing = policy.missing_languages(group.iter().copied());
            if !missing.is_empty() {
                let list: Vec<&str> = missing.iter().map(|l| l.as_str()).collect();
                findings.push(Finding::blocking(
                    "language-missing",
                    None,
                    Some(item.id.clone()),
                    format!("the item has no version in {}", list.join(", ")),
                ));
            }
        }
    }
    CheckResult::from_findings(GateCheck::RequiredLanguages, findings)
}

fn structured_data(items: &[&ContentItem], files: &BTreeMap<String, Vec<u8>>) -> CheckResult {
    let mut findings = Vec::new();
    let mut push = |item: &ContentItem, rule: &str, message: String| {
        findings.push(Finding::blocking(rule, None, Some(item.id.clone()), message));
    };
    for item in items {
        let str_field = |key: &str| {
            item.fields.get(key).and_then(Value::as_str).filter(|s| !s.trim().is_empty())
        };
        if str_field("title").is_none() {
            push(item, "title-missing", "the item has no title".into());
        }
        match item.kind {
            ContentKind::Product => {
                if let Some(price) = item.fields.get("price") {
                    if !price.as_f64().is_some_and(|p| p >= 0.0) {
                        push(
                            item,
                            "price-invalid",
                            "price must be a number of zero or more".into(),
                        );
                    }
                    let currency_ok = str_field("currency").is_some_and(|c| {
                        c.len() == 3 && c.chars().all(|ch| ch.is_ascii_uppercase())
                    });
                    if !currency_ok && !item.is_volatile("price") {
                        push(
                            item,
                            "currency-invalid",
                            "a price needs an ISO 4217 currency such as KES".into(),
                        );
                    }
                }
            }
            ContentKind::Event => {
                let start = item.fields.get("startsAt").and_then(Value::as_str);
                let parse = |s: &str| {
                    chrono::DateTime::parse_from_rfc3339(s).map(|d| d.timestamp()).ok().or_else(
                        || {
                            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok().map(|d| {
                                d.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc().timestamp()
                            })
                        },
                    )
                };
                match start.and_then(parse) {
                    None => push(
                        item,
                        "start-invalid",
                        "an event needs startsAt as an RFC 3339 time or a date".into(),
                    ),
                    Some(starts) => {
                        if let Some(ends) =
                            item.fields.get("endsAt").and_then(Value::as_str).and_then(parse)
                            && ends < starts
                        {
                            push(item, "end-before-start", "endsAt is before startsAt".into());
                        }
                    }
                }
            }
            ContentKind::Article if item.state == ContentState::Corrected => {
                let has_note =
                    item.fields.get("corrections").and_then(Value::as_array).is_some_and(|c| {
                        c.iter().any(|n| {
                            n.get("note")
                                .and_then(Value::as_str)
                                .is_some_and(|t| !t.trim().is_empty())
                        })
                    });
                if !has_note {
                    push(
                        item,
                        "correction-note-missing",
                        "a corrected article needs a visible correction note".into(),
                    );
                }
            }
            _ => {}
        }
    }
    for (path, bytes) in files.iter().filter(|(p, _)| is_html(p)) {
        let document = Html::parse_document(&String::from_utf8_lossy(bytes));
        for block in html::json_ld(&document) {
            if serde_json::from_str::<Value>(&block).is_err() {
                findings.push(Finding::blocking(
                    "json-ld-invalid",
                    Some(path.clone()),
                    None,
                    "a JSON-LD block does not parse".into(),
                ));
            }
        }
    }
    CheckResult::from_findings(GateCheck::StructuredData, findings)
}
