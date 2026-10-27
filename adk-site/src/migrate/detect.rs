//! Detectors with evidence (spec 3.5 §7.8 "Sanitiser", S3): hidden-link spam, cloaking,
//! executables and MIME mismatches among uploads, obfuscated scripts.
//!
//! | Detector | Finds | Evidence |
//! |----------|-------|----------|
//! | Hidden elements | Links to other sites inside elements hidden by a style or the `hidden` attribute; a style block hides by a context-free selector no other rule shows again | How it was hidden, the links, an excerpt |
//! | Link spam | Links whose text or target trips the spam [`ContentFilter`] (`adk-guardrail`) | The link and the filter's reason |
//! | Cloaking | A page that differs when fetched as the crawler and as a browser: scripts, links, redirects, text | The difference, both digests; needs a proven domain (A-I9) |
//! | Uploads | Server-executable files (A-I1), active SVG, a type that does not match the name or the declared type | The rule, the sniffed and the expected type |
//! | Obfuscated scripts | `eval` over decoded strings, `document.write(unescape(…))`, `_0x` identifiers, long encoded literals | The markers and an excerpt |
//!
//! Every finding becomes a quarantined item ([`super::quarantine::QuarantineBook`]).

use std::collections::BTreeSet;
use std::sync::OnceLock;

use adk_core::Content;
use adk_guardrail::{ContentFilter, ContentFilterConfig, Guardrail, GuardrailResult, Severity};
use awp_types::Digest;
use regex::Regex;
use scraper::{Html, Node};
use serde::{Deserialize, Serialize};
use url::Url;

use super::SourceContext;
use super::error::Result;
use super::html::{HiddenRules, analyse};
use super::http::FetchOptions;
use super::inventory::{InventoryMedia, SiteInventory};
use crate::release::file_digest;
use crate::scan::{Sniffed, scan_file, sniff};

/// What a detector found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FindingKind {
    /// Links inside hidden elements.
    HiddenLinks,
    /// Links the spam filter flags.
    SpamLinks,
    /// Different content for the crawler and for a browser.
    Cloaking,
    /// A server-executable or active upload (A-I1); never releasable.
    Executable,
    /// An upload whose content does not match its name or declared type.
    MimeMismatch,
    /// An obfuscated script.
    ObfuscatedScript,
}

impl FindingKind {
    /// Whether an owner's decision can release the subject: never for executables (A-I1).
    pub fn releasable(self) -> bool {
        self != Self::Executable
    }
}

/// What a finding is about.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "id")]
pub enum Subject {
    /// An inventory item, by key.
    Item(String),
    /// A media file, by path.
    Media(String),
    /// A page or file, by path.
    Url(String),
}

/// The evidence behind a finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    /// Where it was seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// What the detector saw.
    pub detail: String,
    /// A short excerpt.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub excerpt: String,
    /// The digest of the content it was seen in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<Digest>,
}

/// A detector's finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    /// What was found.
    pub kind: FindingKind,
    /// What it is about.
    pub subject: Subject,
    /// The evidence.
    pub evidence: Vec<Evidence>,
}

/// Words of the link-spam injected into compromised sites.
pub const DEFAULT_SPAM_KEYWORDS: &[&str] = &[
    "casino",
    "online casino",
    "poker",
    "sports betting",
    "betting tips",
    "viagra",
    "cialis",
    "levitra",
    "payday loan",
    "payday loans",
    "replica watches",
    "essay writing service",
    "porn",
    "xxx",
    "escort",
    "escorts",
    "buy followers",
    "free spins",
    "slot gacor",
    "judi online",
    "togel",
    "crypto giveaway",
    "online pharmacy",
    "weight loss pills",
];

/// What [`Detectors::scan_cloaking`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloakingScan {
    /// Pages served differently to the crawler and to a browser.
    pub findings: Vec<Finding>,
    /// Pages fetched twice.
    pub checked: usize,
    /// What was not checked, and why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// The detectors, with their spam filter.
pub struct Detectors {
    spam: ContentFilter,
}

impl std::fmt::Debug for Detectors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Detectors").field("spam", &self.spam.name()).finish()
    }
}

impl Default for Detectors {
    fn default() -> Self {
        Self::new()
    }
}

fn excerpt(text: &str) -> String {
    super::html::collapse(text).chars().take(160).collect()
}

impl Detectors {
    /// Detectors with the [`DEFAULT_SPAM_KEYWORDS`] filter.
    pub fn new() -> Self {
        let filter = ContentFilter::new(
            "migration_link_spam",
            ContentFilterConfig {
                blocked_keywords: DEFAULT_SPAM_KEYWORDS.iter().map(|k| (*k).to_string()).collect(),
                severity: Severity::High,
                ..Default::default()
            },
        )
        .expect("the default spam keywords compile");
        Self { spam: filter }
    }

    /// Detectors with another spam filter.
    pub fn with_spam_filter(filter: ContentFilter) -> Self {
        Self { spam: filter }
    }

    async fn spam_reason(&self, text: &str) -> Option<String> {
        match self.spam.validate(&Content::new("user").with_text(text)).await {
            GuardrailResult::Fail { reason, .. } => Some(reason),
            _ => None,
        }
    }

    /// Scans an HTML page or body for hidden links, link spam and obfuscated scripts.
    pub async fn scan_html(
        &self,
        subject: &Subject,
        url: Option<&str>,
        html: &str,
    ) -> Vec<Finding> {
        let digest = Some(file_digest(html.as_bytes()));
        let evidence = |detail: String, excerpt_text: &str| Evidence {
            url: url.map(str::to_string),
            detail,
            excerpt: excerpt(excerpt_text),
            digest: digest.clone(),
        };
        let page = url.and_then(|u| Url::parse(u).ok());
        let mut findings = Vec::new();
        let (hidden_links, links, scripts) = {
            let document = Html::parse_document(html);
            let rules = HiddenRules::from_document(&document);
            let mut hidden_links: Vec<(String, Vec<String>, String)> = Vec::new();
            for node in document.tree.nodes() {
                let Node::Element(element) = node.value() else { continue };
                let Some(how) = rules.hides(element) else { continue };
                // Report the outermost hidden element only.
                let nested = node.ancestors().any(|a| match a.value() {
                    Node::Element(e) => rules.hides(e).is_some(),
                    _ => false,
                });
                if nested {
                    continue;
                }
                let mut hrefs = Vec::new();
                let mut text = String::new();
                for descendant in node.descendants() {
                    match descendant.value() {
                        Node::Element(e) if e.name() == "a" => {
                            // Hidden links within the site are page states — skip links,
                            // menus, a cart link shown once something is added — while
                            // injected link spam points at other sites.
                            if let Some(href) = e.attr("href")
                                && leaves_site(href, page.as_ref())
                            {
                                hrefs.push(href.to_string());
                            }
                        }
                        Node::Text(t) => {
                            text.push_str(t);
                            text.push(' ');
                        }
                        _ => {}
                    }
                }
                if !hrefs.is_empty() {
                    hidden_links.push((how, hrefs, text));
                }
            }
            let mut links: Vec<(String, String)> = Vec::new();
            for node in document.tree.nodes() {
                let Node::Element(element) = node.value() else { continue };
                if element.name() != "a" {
                    continue;
                }
                let Some(href) = element.attr("href") else { continue };
                let text: String = node
                    .descendants()
                    .filter_map(|d| match d.value() {
                        Node::Text(t) => Some(t.to_string()),
                        _ => None,
                    })
                    .collect();
                links.push((href.to_string(), super::html::collapse(&text)));
            }
            let mut scripts: Vec<String> = Vec::new();
            for node in document.tree.nodes() {
                let Node::Element(element) = node.value() else { continue };
                if element.name() != "script" {
                    continue;
                }
                let text: String = node
                    .descendants()
                    .filter_map(|d| match d.value() {
                        Node::Text(t) => Some(t.to_string()),
                        _ => None,
                    })
                    .collect();
                if !text.trim().is_empty() {
                    scripts.push(text);
                }
            }
            (hidden_links, links, scripts)
        };
        for (how, hrefs, text) in hidden_links {
            findings.push(Finding {
                kind: FindingKind::HiddenLinks,
                subject: subject.clone(),
                evidence: vec![evidence(
                    format!("{} link(s) hidden by {how}: {}", hrefs.len(), hrefs.join(", ")),
                    &text,
                )],
            });
        }
        let mut spam = Vec::new();
        for (href, text) in &links {
            let target = page.as_ref().and_then(|p| p.join(href).ok());
            let external = match (&target, &page) {
                (Some(t), Some(p)) => t.host_str() != p.host_str(),
                _ => true,
            };
            if !external {
                continue;
            }
            if let Some(reason) = self.spam_reason(&format!("{text} {href}")).await {
                spam.push(evidence(format!("link to {href}: {reason}"), text));
            }
        }
        if !spam.is_empty() {
            findings.push(Finding {
                kind: FindingKind::SpamLinks,
                subject: subject.clone(),
                evidence: spam,
            });
        }
        for script in scripts {
            if let Some(markers) = obfuscation_markers(&script) {
                findings.push(Finding {
                    kind: FindingKind::ObfuscatedScript,
                    subject: subject.clone(),
                    evidence: vec![evidence(
                        format!("inline script: {}", markers.join(", ")),
                        &script,
                    )],
                });
            }
        }
        findings
    }

    /// Scans a script file.
    pub fn scan_script(&self, subject: &Subject, url: Option<&str>, text: &str) -> Option<Finding> {
        obfuscation_markers(text).map(|markers| Finding {
            kind: FindingKind::ObfuscatedScript,
            subject: subject.clone(),
            evidence: vec![Evidence {
                url: url.map(str::to_string),
                detail: format!("script: {}", markers.join(", ")),
                excerpt: excerpt(text),
                digest: Some(file_digest(text.as_bytes())),
            }],
        })
    }

    /// Scans an upload: server-executable files (A-I1), active SVG, and content that does not
    /// match the name or the declared type. Without bytes, only the name is checked.
    pub fn scan_upload(&self, media: &InventoryMedia) -> Vec<Finding> {
        let subject = Subject::Media(media.path.clone());
        let bytes = media.bytes.as_deref().unwrap_or_default();
        let digest = media.bytes.as_ref().map(|b| file_digest(b));
        let evidence = |detail: String, text: &str| Evidence {
            url: Some(media.url.clone()),
            detail,
            excerpt: excerpt(text),
            digest: digest.clone(),
        };
        let mut findings = Vec::new();
        let server_code = scan_file(media.path.trim_start_matches('/'), bytes);
        if !server_code.is_empty() {
            let details: Vec<String> =
                server_code.iter().map(|f| format!("{:?}", f.kind)).collect();
            findings.push(Finding {
                kind: FindingKind::Executable,
                subject: subject.clone(),
                evidence: vec![evidence(
                    format!("server code (A-I1): {}", details.join(", ")),
                    &String::from_utf8_lossy(&bytes[..bytes.len().min(160)]),
                )],
            });
        }
        let sniffed = sniff(bytes);
        if matches!(sniffed, Some(Sniffed::Svg))
            || media.path.to_ascii_lowercase().ends_with(".svg")
        {
            static HANDLER: OnceLock<Regex> = OnceLock::new();
            let handler =
                HANDLER.get_or_init(|| Regex::new(r#"\son[a-z]+\s*="#).expect("static regex"));
            let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
            if text.contains("<script") || text.contains("javascript:") || handler.is_match(&text) {
                findings.push(Finding {
                    kind: FindingKind::Executable,
                    subject: subject.clone(),
                    evidence: vec![evidence("SVG with script or event handlers".into(), &text)],
                });
            }
        }
        if media.bytes.is_some() {
            let expected = expected_type(&media.path);
            let declared = media
                .declared_type
                .as_deref()
                .map(|t| t.split(';').next().unwrap_or_default().trim().to_ascii_lowercase());
            let sniffed_type = sniffed.map(Sniffed::content_type);
            let mut problems = Vec::new();
            if let Some(expected) = expected
                && sniffed_type != Some(expected)
                && !(expected == "image/svg+xml" && sniffed.is_none() && looks_like_svg(bytes))
            {
                problems.push(format!(
                    "the name says {expected}, the content is {}",
                    sniffed_type.unwrap_or("not a recognised format")
                ));
            }
            if let (Some(declared), Some(sniffed_type)) = (&declared, sniffed_type)
                && declared.starts_with("image/") != sniffed_type.starts_with("image/")
            {
                problems.push(format!("declared {declared}, the content is {sniffed_type}"));
            }
            if !problems.is_empty() && server_code.is_empty() {
                findings.push(Finding {
                    kind: FindingKind::MimeMismatch,
                    subject,
                    evidence: vec![evidence(
                        problems.join("; "),
                        &String::from_utf8_lossy(&bytes[..bytes.len().min(160)]),
                    )],
                });
            }
        }
        findings
    }

    /// Fetches `url` as the crawler and as a browser and reports what differs (spec 3.5 §7.8
    /// "cloaking (dual fetch as browser and crawler, then a diff)").
    ///
    /// # Errors
    ///
    /// [`MigrateError::NotProven`](super::MigrateError::NotProven) unless the domain is
    /// proven (A-I9); HTTP errors.
    pub async fn check_cloaking(
        &self,
        ctx: &SourceContext,
        subject: &Subject,
        url: &Url,
    ) -> Result<Option<Finding>> {
        ctx.require_proven(url.host_str().unwrap_or_default(), "a cloaking check")?;
        let crawler = ctx.http.fetch(url, &FetchOptions::crawler().following(3)).await?;
        let browser = ctx.http.fetch(url, &FetchOptions::browser().following(3)).await?;
        let mut differences = Vec::new();
        if crawler.status != browser.status || crawler.url != browser.url {
            differences.push(format!(
                "the crawler got {} at {}, a browser {} at {}",
                crawler.status, crawler.url, browser.status, browser.url
            ));
        }
        let a = analyse(&crawler.text(), &crawler.url);
        let b = analyse(&browser.text(), &browser.url);
        let scripts = |page: &super::html::PageAnalysis| -> BTreeSet<String> {
            page.scripts
                .iter()
                .map(|s| match (&s.src, &s.inline) {
                    (Some(src), _) => {
                        format!("src {}{}", src.origin().ascii_serialization(), src.path())
                    }
                    (None, Some(inline)) => format!("inline {}", script_shape(inline)),
                    (None, None) => String::new(),
                })
                .collect()
        };
        let (crawler_scripts, browser_scripts) = (scripts(&a), scripts(&b));
        let only_crawler: Vec<&String> = crawler_scripts.difference(&browser_scripts).collect();
        let only_browser: Vec<&String> = browser_scripts.difference(&crawler_scripts).collect();
        if !only_crawler.is_empty() {
            differences
                .push(format!("scripts served only to the crawler: {}", join_short(&only_crawler)));
        }
        if !only_browser.is_empty() {
            differences
                .push(format!("scripts served only to browsers: {}", join_short(&only_browser)));
        }
        let external = |page: &super::html::PageAnalysis, host: Option<&str>| -> BTreeSet<String> {
            page.links.iter().filter(|l| l.host_str() != host).map(|l| l.to_string()).collect()
        };
        let host = url.host_str();
        let (crawler_links, browser_links) = (external(&a, host), external(&b, host));
        let crawler_only_links: Vec<&String> = crawler_links.difference(&browser_links).collect();
        if !crawler_only_links.is_empty() {
            differences.push(format!(
                "links served only to the crawler: {}",
                join_short(&crawler_only_links)
            ));
        }
        let similarity = similar::TextDiff::configure()
            .timeout(std::time::Duration::from_secs(2))
            .diff_words(&a.meta.main_text, &b.meta.main_text)
            .ratio();
        if similarity < 0.9 {
            differences
                .push(format!("main text similarity {similarity:.2} between the two fetches"));
        }
        if differences.is_empty() {
            return Ok(None);
        }
        tracing::warn!(url = %url, differences = differences.len(), "cloaking detected");
        Ok(Some(Finding {
            kind: FindingKind::Cloaking,
            subject: subject.clone(),
            evidence: vec![
                Evidence {
                    url: Some(url.to_string()),
                    detail: differences.join("; "),
                    excerpt: excerpt(&a.meta.main_text),
                    digest: Some(file_digest(&crawler.body)),
                },
                Evidence {
                    url: Some(url.to_string()),
                    detail: "as a browser".into(),
                    excerpt: excerpt(&b.meta.main_text),
                    digest: Some(file_digest(&browser.body)),
                },
            ],
        }))
    }

    /// Runs [`check_cloaking`](Self::check_cloaking) on the home page and the top `limit`
    /// pages of `inventory` (spec 3.5 §7.8 "Sanitiser", S3). The dual fetch reads the live
    /// site, so it runs only for a proven domain (A-I9): otherwise nothing is fetched and the
    /// scan says why. A page that cannot be fetched is noted, not fatal.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_site::migrate::detect::Detectors;
    ///
    /// # async fn demo(ctx: &adk_site::migrate::SourceContext, inventory: &adk_site::migrate::SiteInventory) {
    /// let scan = Detectors::new().scan_cloaking(ctx, inventory, 20).await;
    /// for finding in &scan.findings {
    ///     println!("{:?}: {}", finding.subject, finding.evidence[0].detail);
    /// }
    /// # }
    /// ```
    pub async fn scan_cloaking(
        &self,
        ctx: &SourceContext,
        inventory: &SiteInventory,
        limit: usize,
    ) -> CloakingScan {
        let mut scan = CloakingScan::default();
        let host = inventory
            .origin
            .as_deref()
            .and_then(|origin| Url::parse(origin).ok())
            .and_then(|url| url.host_str().map(str::to_string))
            .or_else(|| {
                inventory
                    .urls
                    .first()
                    .and_then(|u| Url::parse(&u.url).ok())
                    .and_then(|u| u.host_str().map(str::to_string))
            });
        let Some(host) = host else {
            scan.notes.push("cloaking not checked: the inventory names no site".into());
            return scan;
        };
        if let Err(error) = ctx.require_proven(&host, "a cloaking check") {
            scan.notes.push(format!("cloaking not checked: {error}"));
            return scan;
        }
        for url in inventory.top_pages(limit) {
            let Ok(target) = Url::parse(&url.url) else { continue };
            let subject = match &url.item {
                Some(key) => Subject::Item(key.clone()),
                None => Subject::Url(url.path.clone()),
            };
            scan.checked += 1;
            match self.check_cloaking(ctx, &subject, &target).await {
                Ok(Some(finding)) => scan.findings.push(finding),
                Ok(None) => {}
                Err(error) => {
                    scan.notes.push(format!("{}: cloaking not checked: {error}", url.path))
                }
            }
        }
        tracing::info!(
            checked = scan.checked,
            findings = scan.findings.len(),
            "migration cloaking check ran"
        );
        scan
    }

    /// Scans a whole inventory: every page body read, every item body, every upload and
    /// every script file.
    pub async fn scan_inventory(&self, inventory: &SiteInventory) -> Vec<Finding> {
        let mut findings: Vec<Finding> = Vec::new();
        let mut scanned_items = BTreeSet::new();
        for url in &inventory.urls {
            let Some(body) = &url.body else { continue };
            let subject = match &url.item {
                Some(key) => {
                    scanned_items.insert(key.clone());
                    Subject::Item(key.clone())
                }
                None => Subject::Url(url.path.clone()),
            };
            let content_type = url.content_type.as_deref().unwrap_or_default();
            let text = String::from_utf8_lossy(body);
            if content_type.contains("html") {
                findings.extend(self.scan_html(&subject, Some(&url.url), &text).await);
            } else if content_type.contains("javascript") || url.path.ends_with(".js") {
                findings.extend(self.scan_script(&subject, Some(&url.url), &text));
            }
        }
        for item in &inventory.items {
            if scanned_items.contains(&item.key) {
                continue;
            }
            let subject = Subject::Item(item.key.clone());
            findings.extend(self.scan_html(&subject, item.url.as_deref(), &item.html).await);
        }
        for media in &inventory.media {
            findings.extend(self.scan_upload(media));
        }
        let mut seen = BTreeSet::new();
        findings.retain(|f| {
            seen.insert((f.kind, f.subject.clone(), f.evidence.first().map(|e| e.detail.clone())))
        });
        tracing::info!(findings = findings.len(), "migration detectors ran");
        findings
    }
}

/// Whether `href` leads to another host than the page's.
fn leaves_site(href: &str, page: Option<&Url>) -> bool {
    let target = match page {
        Some(page) => page.join(href),
        None => Url::parse(href),
    };
    target.is_ok_and(|t| {
        matches!(t.scheme(), "http" | "https") && t.host_str() != page.and_then(Url::host_str)
    })
}

fn join_short(values: &[&String]) -> String {
    let mut out: Vec<String> =
        values.iter().take(5).map(|v| v.chars().take(120).collect()).collect();
    if values.len() > 5 {
        out.push(format!("and {} more", values.len() - 5));
    }
    out.join(", ")
}

/// An inline script reduced to its shape: long alphanumeric runs (nonces, ids) replaced.
fn script_shape(script: &str) -> String {
    static RUN: OnceLock<Regex> = OnceLock::new();
    let run = RUN.get_or_init(|| Regex::new(r"[A-Za-z0-9]{7,}").expect("static regex"));
    let shape = run.replace_all(&super::html::collapse(script), "#").into_owned();
    shape.chars().take(200).collect()
}

/// The markers of an obfuscated script, when there are enough of them.
pub fn obfuscation_markers(script: &str) -> Option<Vec<String>> {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str, bool)>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            (r"eval\s*\(\s*(?:atob|unescape|decodeURIComponent|String\.fromCharCode|function\s*\(p,\s*a,\s*c,\s*k)", "eval over decoded text", true),
            (r"document\.write\s*\(\s*(?:unescape|atob|decodeURIComponent)\s*\(", "document.write of decoded text", true),
            (r"new\s+Function\s*\(\s*(?:atob|unescape)", "Function over decoded text", true),
            (r"\b_0x[0-9a-f]{4,}\b", "_0x identifiers", false),
            (r"(?:\\x[0-9a-fA-F]{2}){20,}", "long hex escape runs", false),
            (r"[A-Za-z0-9+/]{240,}={0,2}", "long encoded literal", false),
            (r"String\.fromCharCode\s*\(\s*(?:\d+\s*,\s*){15,}", "character-code strings", false),
            (r"\batob\s*\(", "atob", false),
        ]
        .into_iter()
        .filter_map(|(pattern, label, strong)| Regex::new(pattern).ok().map(|r| (r, label, strong)))
        .collect()
    });
    let mut markers = Vec::new();
    let mut strong = false;
    for (pattern, label, is_strong) in patterns {
        let count = pattern.find_iter(script).count();
        let counts = if *label == "_0x identifiers" { count >= 5 } else { count > 0 };
        if counts {
            markers.push((*label).to_string());
            strong |= *is_strong;
        }
    }
    (strong || markers.len() >= 2).then_some(markers)
}

fn expected_type(path: &str) -> Option<&'static str> {
    let extension = path.rsplit('/').next()?.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match extension.as_str() {
        "jpg" | "jpeg" | "jpe" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "mp4" | "m4v" | "mov" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        "woff" | "woff2" => "font/woff2",
        "svg" => "image/svg+xml",
        _ => return None,
    })
}

fn looks_like_svg(bytes: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_ascii_lowercase();
    head.contains("<svg")
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn obfuscation_needs_a_strong_marker_or_two_weak_ones() {
        assert!(obfuscation_markers("eval(atob('Y29uc29sZS5sb2coMSk='))").is_some());
        assert!(obfuscation_markers("document.write(unescape('%3Cb%3E'))").is_some());
        assert!(
            obfuscation_markers("var _0x1a2b=1,_0x1a2c=2,_0x1a2d=3,_0x1a2e=4,_0x1a2f=5; atob(x)")
                .is_some()
        );
        assert!(obfuscation_markers("console.log('hello'); var x = atob(y);").is_none());
        assert!(obfuscation_markers("jQuery(function($){ $('.menu').toggle(); });").is_none());
    }

    #[test]
    fn uploads_are_checked_by_content() {
        let detectors = Detectors::new();
        let php = InventoryMedia::new(
            "https://o.test/wp-content/uploads/cache.php",
            "/wp-content/uploads/cache.php",
        )
        .with_bytes(Bytes::from_static(b"<?php /* inert */ ?>"));
        assert_eq!(detectors.scan_upload(&php)[0].kind, FindingKind::Executable);
        let fake = InventoryMedia::new("https://o.test/u/photo.jpg", "/u/photo.jpg")
            .with_bytes(Bytes::from_static(b"<html><body>not an image</body></html>"));
        assert_eq!(detectors.scan_upload(&fake)[0].kind, FindingKind::MimeMismatch);
        let png = InventoryMedia::new("https://o.test/u/a.png", "/u/a.png")
            .with_bytes(Bytes::from_static(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"));
        assert!(detectors.scan_upload(&png).is_empty());
        let svg = InventoryMedia::new("https://o.test/u/a.svg", "/u/a.svg").with_bytes(
            Bytes::from_static(b"<svg xmlns='http://www.w3.org/2000/svg' onload='x()'></svg>"),
        );
        assert_eq!(detectors.scan_upload(&svg)[0].kind, FindingKind::Executable);
        assert!(!FindingKind::Executable.releasable());
    }

    #[tokio::test]
    async fn hidden_and_spam_links_are_found() {
        let detectors = Detectors::new();
        let subject = Subject::Item("wp:post:1".into());
        let findings = detectors
            .scan_html(
                &subject,
                Some("https://o.test/hello/"),
                r#"<p>Hello <a href="/about/">about</a></p>
                   <div style="position:absolute;left:-9999px"><a href="https://cheap-casino.test/">best online casino</a></div>
                   <p><a href="https://partner.test/">our partner</a></p>"#,
            )
            .await;
        let kinds: Vec<FindingKind> = findings.iter().map(|f| f.kind).collect();
        assert_eq!(kinds, [FindingKind::HiddenLinks, FindingKind::SpamLinks]);
        assert!(findings[0].evidence[0].detail.contains("cheap-casino.test"));
        assert!(findings[0].evidence[0].detail.contains("off-screen"));
    }

    /// The markup a current WordPress site serves (spec 3.5 §11 "Fixtures"): the navigation
    /// block's inline styles hide its responsive container and submenus until a viewport, a
    /// hover or the menu button shows them; the skip link is screen-reader text pointing into
    /// the page; WooCommerce's "View cart" link waits, `hidden`, for the first item. None of
    /// it is hidden-link spam; hidden links to other sites still are.
    #[tokio::test]
    async fn theme_menus_skip_links_and_cart_links_are_not_hidden_links() {
        const HEAD: &str = r##"<!doctype html><html><head>
            <style id="wp-block-navigation-inline-css">
            .wp-block-navigation__container .is-responsive{display:none}
            .wp-block-navigation .has-child .wp-block-navigation__submenu-container{height:0;opacity:0;overflow:hidden;visibility:hidden;width:0}
            .wp-block-navigation .has-child:hover>.wp-block-navigation__submenu-container{height:auto;opacity:1;visibility:visible}
            .wp-block-navigation__responsive-container{bottom:0;display:none;left:0;position:fixed;right:0;top:0}
            .wp-block-navigation__responsive-container.is-menu-open{display:flex}
            @media (min-width:600px){.wp-block-navigation__responsive-container:not(.hidden-by-default):not(.is-menu-open){display:block;position:relative}}
            @media (max-width:599px){.desktop-only{display:none}}
            @keyframes overlay-menu__fade-in-animation{0%{opacity:0}to{opacity:1}}
            </style>
            <style>/* screen-reader text */ .sr-only{position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0,0,0,0)}
            .skip-link.screen-reader-text{clip-path:inset(50%);height:1px;overflow:hidden;position:absolute;width:1px}
            .sneaky,#promo{display:none}</style></head><body>
            <a class="skip-link screen-reader-text" href="#wp--skip-link--target">Skip to content</a>
            <nav class="is-responsive wp-block-navigation"><div class="wp-block-navigation__responsive-container" id="modal-1">
              <ul class="wp-block-navigation__container"><li class="has-child"><a href="/about/">About</a>
                <ul class="wp-block-navigation__submenu-container"><li><a href="/about/team/">Team</a></li></ul></li></ul></div></nav>
            <div class="desktop-only"><a href="/shop/">Shop</a></div>
            <div class="sr-only"><a href="/contact/">Contact us</a> <a href="https://o.test/hello/#top">Top</a></div>
            <p style="display:none"><a href="#comments">Comments</a></p>
            <a href="https://o.test/cart/" class="added_to_cart wc-forward" hidden>View cart</a>
            <main id="wp--skip-link--target"><p>Text.</p></main>"##;
        let detectors = Detectors::new();
        let subject = Subject::Url("/hello/".into());
        let page = Some("https://o.test/hello/");
        let clean = detectors.scan_html(&subject, page, &format!("{HEAD}</body></html>")).await;
        assert_eq!(clean, Vec::new());

        let infected = format!(
            r#"{HEAD}<div class="sneaky"><a href="https://cheap-casino.test/">casino</a></div>
            <section id="promo"><a href="/ok/">ok</a> <a href="https://bonus.test/">bonus</a></section>
            <div class="sr-only"><a href="https://pills.test/">pills</a></div></body></html>"#
        );
        let found = detectors.scan_html(&subject, page, &infected).await;
        let hidden: Vec<&str> = found
            .iter()
            .filter(|f| f.kind == FindingKind::HiddenLinks)
            .map(|f| f.evidence[0].detail.as_str())
            .collect();
        assert_eq!(
            hidden,
            [
                "1 link(s) hidden by .sneaky hidden by a style block: https://cheap-casino.test/",
                "1 link(s) hidden by #promo hidden by a style block: https://bonus.test/",
                "1 link(s) hidden by .sr-only hidden by a style block: https://pills.test/",
            ]
        );
    }
}
