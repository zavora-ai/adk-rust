//! Migration parity for the launch gate (spec 3.5 §7.8.1 "Migration parity", S3): every
//! inventoried URL answers 200 or a 301 to a 200, the top pages say the same (main-text
//! similarity at or above 0.95) and keep their metadata, and — when the owner keeps the site's
//! design — look the same at two widths (perceptual difference under 0.05).
//!
//! | Check | Rule | Finding | Gates the launch |
//! |-------|------|---------|------------------|
//! | URL coverage | A file for the path, or a 301/308 redirect to one; refused and quarantined subjects are exempt | `url-not-covered` | Always |
//! | Main text | Word-level similarity of the main content ≥ `min_text_similarity` | `text-similarity` | Always |
//! | Metadata | Title, canonical path and language kept; a description kept when there was one | `metadata-*` | Always |
//! | Visual | 1 − the hybrid SSIM/RMS score of two screenshots < `max_perceptual_difference`, at each width | `perceptual-difference` | [`SiteDesign::Keep`] only; reported for [`SiteDesign::NewTheme`] |
//!
//! The owner chooses the design during migration (owner decision of 2026-10-25):
//! [`SiteDesign::Keep`] for a freeze or the old design carried over, [`SiteDesign::NewTheme`]
//! for a new look the owner approves from the preview. [`record_design_choice`] writes the
//! choice to the migration audit.
//!
//! Screenshots come from a [`Screenshotter`]: [`ChromeScreenshotter`] runs headless Chrome;
//! without one the visual comparison is reported as not measured.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use adk_core::ActingContext;
use adk_core::audit_log::{AuditDecision, AuditRecorder};
use async_trait::async_trait;
use awp_types::Redirect;
use image::RgbImage;
use serde::{Deserialize, Serialize};
use url::Url;

use super::audit::record;
use super::error::{MigrateError, Result};
use super::html::main_text;
use super::http::{FetchOptions, HttpClient};
use super::inventory::{InventoryUrl, PageMeta, SiteInventory};
use super::serve::StaticServer;
use crate::gate::{
    CheckResult, CheckStatus, Finding, GateCheck, GateExtension, GateInput, GateMode, Severity,
};

/// The design the owner chose for the migrated site (spec 3.5 §7.8.1, §7.18 stage 6; owner
/// decision of 2026-10-25).
///
/// # Example
///
/// ```rust
/// use adk_site::migrate::parity::{ParityConfig, SiteDesign};
///
/// assert!(SiteDesign::Keep.gates_looks());
/// assert!(!ParityConfig::new_theme().design.gates_looks());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SiteDesign {
    /// Keep the same design — a freeze, or the old design carried over: URL coverage, main
    /// text, metadata and the perceptual difference all gate the launch.
    #[default]
    Keep,
    /// Move to a new theme: URL coverage, main text and metadata gate the launch; the
    /// perceptual difference is reported, not gated, and the owner approves the new look from
    /// the preview.
    NewTheme,
}

impl SiteDesign {
    /// Whether the perceptual difference gates the launch.
    pub fn gates_looks(self) -> bool {
        self == Self::Keep
    }

    /// The wire name: `keep` or `new_theme`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::NewTheme => "new_theme",
        }
    }
}

impl std::fmt::Display for SiteDesign {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for SiteDesign {
    type Err = MigrateError;

    fn from_str(text: &str) -> Result<Self> {
        match text.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "keep" | "keep_design" | "same" => Ok(Self::Keep),
            "new_theme" | "new" | "theme" => Ok(Self::NewTheme),
            other => Err(MigrateError::Service(format!(
                "unknown design choice '{other}'; use `keep` or `new-theme`"
            ))),
        }
    }
}

/// The parity thresholds and the owner's design choice (spec 3.5 §7.8.1).
#[derive(Debug, Clone, PartialEq)]
pub struct ParityConfig {
    /// Pages compared for text, metadata and looks (default 20).
    pub top_n: usize,
    /// Viewport widths screenshots are taken at (default 1280 and 390).
    pub widths: Vec<u32>,
    /// Viewport height (default 1600).
    pub height: u32,
    /// The largest perceptual difference allowed (default 0.05).
    pub max_perceptual_difference: f64,
    /// The smallest main-text similarity allowed (default 0.95).
    pub min_text_similarity: f64,
    /// The owner's design choice: whether the perceptual difference gates the launch
    /// (default [`SiteDesign::Keep`]).
    pub design: SiteDesign,
}

impl Default for ParityConfig {
    fn default() -> Self {
        Self {
            top_n: 20,
            widths: vec![1280, 390],
            height: 1600,
            max_perceptual_difference: 0.05,
            min_text_similarity: 0.95,
            design: SiteDesign::Keep,
        }
    }
}

impl ParityConfig {
    /// The spec's thresholds for a site that keeps its design: every check gates the launch.
    pub fn keep_design() -> Self {
        Self::default()
    }

    /// The spec's thresholds for a site that moves to a new theme: the perceptual difference
    /// is reported, not gated.
    pub fn new_theme() -> Self {
        Self { design: SiteDesign::NewTheme, ..Self::default() }
    }

    /// The thresholds for `design`.
    pub fn for_design(design: SiteDesign) -> Self {
        Self { design, ..Self::default() }
    }
}

/// The migration-audit fields of the owner's design choice (spec 3.5 §7.8.1, §9.3): the
/// choice, the checks that gate the launch and the thresholds.
///
/// # Example
///
/// ```rust
/// use adk_site::migrate::parity::{ParityConfig, design_choice_fields};
///
/// let fields = design_choice_fields(&ParityConfig::keep_design());
/// assert_eq!(fields["design"], "keep");
/// assert_eq!(fields["gates"][3], "perceptual-difference");
/// ```
pub fn design_choice_fields(config: &ParityConfig) -> serde_json::Value {
    let mut gates = vec!["url-coverage", "text-similarity", "metadata"];
    if config.design.gates_looks() {
        gates.push("perceptual-difference");
    }
    serde_json::json!({
        "decision": "design",
        "design": config.design,
        "gates": gates,
        "maxPerceptualDifference": config.max_perceptual_difference,
        "minTextSimilarity": config.min_text_similarity,
        "lookApprovedFromPreview": !config.design.gates_looks(),
    })
}

/// Records the owner's design choice in the migration audit as `site.migration.decided`
/// with [`design_choice_fields`] (spec 3.5 §7.8.1, §9.3; owner decision of 2026-10-25).
///
/// # Errors
///
/// [`MigrateError::Service`] when the recorder fails.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::audit_log::{AuditRecorder, InMemoryAuditRecorder};
/// use adk_site::migrate::parity::{ParityConfig, record_design_choice};
///
/// # async fn demo(acting: adk_core::ActingContext) -> adk_site::migrate::Result<()> {
/// let recorder = Arc::new(InMemoryAuditRecorder::new());
/// let audit: Arc<dyn AuditRecorder> = recorder.clone();
/// record_design_choice(&audit, &acting, &ParityConfig::new_theme()).await?;
/// assert_eq!(recorder.records()[0].action, "site.migration.decided");
/// # Ok(())
/// # }
/// ```
pub async fn record_design_choice(
    recorder: &Arc<dyn AuditRecorder>,
    acting: &ActingContext,
    config: &ParityConfig,
) -> Result<()> {
    tracing::info!(site.migration.design = %config.design, "migration design chosen");
    record(
        recorder,
        acting,
        "site.migration.decided",
        Some(AuditDecision::Approved),
        design_choice_fields(config),
    )
    .await
}

/// Takes screenshots of pages.
#[async_trait]
pub trait Screenshotter: Send + Sync + std::fmt::Debug {
    /// A `width` × `height` screenshot of `url`.
    ///
    /// # Errors
    ///
    /// [`MigrateError::Screenshot`] when the page cannot be captured.
    async fn screenshot(&self, url: &Url, width: u32, height: u32) -> Result<RgbImage>;
}

/// Headless Chrome through its command line, one process per screenshot, killed on drop.
#[derive(Debug, Clone)]
pub struct ChromeScreenshotter {
    binary: PathBuf,
    timeout: Duration,
}

impl ChromeScreenshotter {
    /// Chrome or Chromium at `binary`.
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self { binary: binary.into(), timeout: Duration::from_secs(60) }
    }

    /// The first Chrome found: `CHROME_PATH`, then the usual install locations.
    pub fn detect() -> Option<Self> {
        let mut candidates: Vec<PathBuf> =
            std::env::var_os("CHROME_PATH").map(PathBuf::from).into_iter().collect();
        candidates.extend(
            [
                "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                "/Applications/Chromium.app/Contents/MacOS/Chromium",
                "/usr/bin/google-chrome",
                "/usr/bin/google-chrome-stable",
                "/usr/bin/chromium",
                "/usr/bin/chromium-browser",
                "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
            ]
            .map(PathBuf::from),
        );
        candidates.into_iter().find(|p| p.is_file()).map(Self::new)
    }
}

#[async_trait]
impl Screenshotter for ChromeScreenshotter {
    async fn screenshot(&self, url: &Url, width: u32, height: u32) -> Result<RgbImage> {
        let dir = tempfile::tempdir().map_err(|e| MigrateError::Screenshot(e.to_string()))?;
        let file = dir.path().join("shot.png");
        let mut command = tokio::process::Command::new(&self.binary);
        command
            .args([
                "--headless=new",
                "--disable-gpu",
                "--hide-scrollbars",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-extensions",
                "--disable-background-networking",
                "--mute-audio",
                "--force-device-scale-factor=1",
                "--virtual-time-budget=3000",
            ])
            .arg(format!("--user-data-dir={}", dir.path().join("profile").display()))
            .arg(format!("--window-size={width},{height}"))
            .arg(format!("--screenshot={}", file.display()))
            .arg(url.as_str())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let status = tokio::time::timeout(self.timeout, command.status())
            .await
            .map_err(|_| MigrateError::Screenshot(format!("{url}: Chrome timed out")))?
            .map_err(|e| MigrateError::Screenshot(format!("Chrome could not run: {e}")))?;
        if !status.success() || !file.exists() {
            return Err(MigrateError::Screenshot(format!("{url}: Chrome exited with {status}")));
        }
        let bytes =
            tokio::fs::read(&file).await.map_err(|e| MigrateError::Screenshot(e.to_string()))?;
        image::load_from_memory(&bytes)
            .map(|i| i.to_rgb8())
            .map_err(|e| MigrateError::Screenshot(format!("{url}: {e}")))
    }
}

/// The perceptual difference of two screenshots: 1 − the hybrid score (SSIM on luma, RMS on
/// chroma) of `image-compare`; 0 for identical images. `b` is scaled to `a`'s size first.
///
/// # Errors
///
/// [`MigrateError::Screenshot`] when the images cannot be compared.
///
/// # Example
///
/// ```rust
/// use adk_site::migrate::parity::perceptual_difference;
/// use image::{Rgb, RgbImage};
///
/// let white = RgbImage::from_pixel(64, 64, Rgb([255, 255, 255]));
/// assert!(perceptual_difference(&white, &white).unwrap() < 1e-9);
/// ```
pub fn perceptual_difference(a: &RgbImage, b: &RgbImage) -> Result<f64> {
    let resized;
    let b = if a.dimensions() == b.dimensions() {
        b
    } else {
        resized = image::imageops::resize(
            b,
            a.width(),
            a.height(),
            image::imageops::FilterType::Triangle,
        );
        &resized
    };
    image_compare::rgb_hybrid_compare(a, b)
        .map(|similarity| (1.0 - similarity.score).max(0.0))
        .map_err(|e| MigrateError::Screenshot(e.to_string()))
}

/// Word-level similarity of two texts, 0 to 1.
///
/// # Example
///
/// ```rust
/// use adk_site::migrate::parity::text_similarity;
///
/// assert_eq!(text_similarity("the same words", "the same words"), 1.0);
/// assert!(text_similarity("one two three four", "five six seven eight") < 0.5);
/// ```
pub fn text_similarity(a: &str, b: &str) -> f64 {
    if a.trim().is_empty() && b.trim().is_empty() {
        return 1.0;
    }
    f64::from(
        similar::TextDiff::configure().timeout(Duration::from_secs(5)).diff_words(a, b).ratio(),
    )
}

/// The page-specific part of a title: the text before the site-name separator.
fn title_core(title: &str) -> String {
    let core = [" · ", " – ", " — ", " - ", " | "]
        .iter()
        .fold(title, |t, sep| t.split(sep).next().unwrap_or(t));
    super::html::collapse(core).to_lowercase()
}

/// The migration-parity [`GateExtension`] (spec 3.5 §7.8.1).
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_site::gate::LaunchGate;
/// use adk_site::migrate::parity::{ChromeScreenshotter, MigrationParity};
/// # fn demo(inventory: adk_site::migrate::SiteInventory) {
/// let mut parity = MigrationParity::new(Arc::new(inventory))
///     .with_legacy("http://127.0.0.1:8081".parse().unwrap());
/// if let Some(chrome) = ChromeScreenshotter::detect() {
///     parity = parity.with_screenshotter(Arc::new(chrome));
/// }
/// let gate = LaunchGate::new().with_extension(Arc::new(parity));
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct MigrationParity {
    inventory: Arc<SiteInventory>,
    excluded: BTreeSet<String>,
    legacy: Option<Url>,
    http: Option<Arc<HttpClient>>,
    screenshotter: Option<Arc<dyn Screenshotter>>,
    preview: Option<Url>,
    config: ParityConfig,
}

impl MigrationParity {
    /// Parity against `inventory` with the spec's thresholds.
    pub fn new(inventory: Arc<SiteInventory>) -> Self {
        Self {
            inventory,
            excluded: BTreeSet::new(),
            legacy: None,
            http: None,
            screenshotter: None,
            preview: None,
            config: ParityConfig::default(),
        }
    }

    /// Exempts refused and quarantined subjects (item keys and paths) from coverage.
    #[must_use]
    pub fn excluding(mut self, subjects: impl IntoIterator<Item = String>) -> Self {
        self.excluded.extend(subjects);
        self
    }

    /// Where the legacy pages are rendered for screenshots and text: the live site, a
    /// replay or a frozen snapshot.
    #[must_use]
    pub fn with_legacy(mut self, base: Url) -> Self {
        self.legacy = Some(base);
        self
    }

    /// Reads legacy pages' text through `http` when the inventory has none.
    #[must_use]
    pub fn with_http(mut self, http: Arc<HttpClient>) -> Self {
        self.http = Some(http);
        self
    }

    /// Takes screenshots with `screenshotter`.
    #[must_use]
    pub fn with_screenshotter(mut self, screenshotter: Arc<dyn Screenshotter>) -> Self {
        self.screenshotter = Some(screenshotter);
        self
    }

    /// Where the build is served; by default the gate's output directory is served on loopback.
    #[must_use]
    pub fn with_preview(mut self, origin: Url) -> Self {
        self.preview = Some(origin);
        self
    }

    /// Sets the thresholds and the design choice.
    #[must_use]
    pub fn with_config(mut self, config: ParityConfig) -> Self {
        self.config = config;
        self
    }

    /// Sets the owner's design choice, keeping the thresholds (owner decision of 2026-10-25).
    #[must_use]
    pub fn with_design(mut self, design: SiteDesign) -> Self {
        self.config.design = design;
        self
    }

    /// The thresholds and the design choice.
    pub fn config(&self) -> &ParityConfig {
        &self.config
    }

    fn exempt(&self, url: &InventoryUrl) -> bool {
        self.excluded.contains(&url.path)
            || url.item.as_ref().is_some_and(|k| self.excluded.contains(k))
    }

    /// The file answering `path` in the build, following one redirect.
    fn answer(out: &Path, path: &str, redirects: &[Redirect]) -> Option<String> {
        let path = path.split('?').next().unwrap_or(path);
        let file = |p: &str| -> Option<String> {
            let relative = p.trim_start_matches('/');
            let direct = if relative.is_empty() || relative.ends_with('/') {
                format!("{relative}index.html")
            } else {
                relative.to_string()
            };
            if out.join(&direct).is_file() {
                return Some(direct);
            }
            let index = format!("{relative}/index.html");
            out.join(&index).is_file().then_some(index)
        };
        if let Some(found) = file(path) {
            return Some(found);
        }
        let rule = redirects.iter().find(|r| r.from == path && matches!(r.status, 301 | 308))?;
        file(&rule.to)
    }

    async fn legacy_meta(&self, url: &InventoryUrl) -> Option<PageMeta> {
        if let Some(page) = url.page.as_ref().filter(|p| !p.main_text.is_empty()) {
            return Some(page.clone());
        }
        let (http, base) = (self.http.as_ref()?, self.legacy.as_ref()?);
        let target = base.join(&url.path).ok()?;
        let fetched = http.fetch(&target, &FetchOptions::crawler().following(3)).await.ok()?;
        (fetched.status == 200).then(|| super::html::analyse(&fetched.text(), &target).meta)
    }
}

#[async_trait]
impl GateExtension for MigrationParity {
    fn check(&self) -> GateCheck {
        GateCheck::MigrationParity
    }

    fn runs_in(&self, mode: GateMode) -> bool {
        mode == GateMode::Full
    }

    async fn run(&self, input: &GateInput<'_>) -> crate::Result<CheckResult> {
        let gates_looks = self.config.design.gates_looks();
        let mut findings = Vec::new();
        let mut finding = |rule: &str, path: &str, message: String| {
            // A new theme's looks are the owner's to approve from the preview: reported only.
            let blocking = rule != "perceptual-difference" || gates_looks;
            findings.push(Finding {
                rule: rule.into(),
                severity: if blocking { Severity::Serious } else { Severity::Minor },
                blocking,
                path: Some(path.to_string()),
                item: None,
                message,
            });
        };
        let mut redirects = input.project.config.redirects.clone();
        redirects.extend(input.report.redirects.iter().cloned());
        let media: BTreeSet<&String> = self.inventory.media.iter().map(|m| &m.path).collect();
        let mut covered = 0usize;
        for url in &self.inventory.urls {
            let counted = (url.status == 200
                && (url.page.is_some() || url.item.is_some() || media.contains(&url.path)))
                || matches!(url.status, 301 | 308);
            if !counted || self.exempt(url) || url.path.contains('?') {
                continue;
            }
            if Self::answer(input.out, &url.path, &redirects).is_some() {
                covered += 1;
            } else {
                finding(
                    "url-not-covered",
                    &url.path,
                    format!("{} answers neither 200 nor a 301 to a 200 on the new site", url.path),
                );
            }
        }
        // Content pages only: listings and archives are rebuilt by the generator, not migrated.
        let mut pages: Vec<&InventoryUrl> = self.inventory.top_pages(usize::MAX);
        pages.retain(|u| u.item.is_some());
        pages.truncate(self.config.top_n);
        if pages.is_empty() {
            let mut items: Vec<&InventoryUrl> = self
                .inventory
                .urls
                .iter()
                .filter(|u| u.status == 200 && u.item.is_some())
                .collect();
            items.sort_by(|a, b| (b.path == "/").cmp(&(a.path == "/")).then(a.path.cmp(&b.path)));
            items.truncate(self.config.top_n);
            pages = items;
        }
        pages.retain(|u| !self.exempt(u));
        let mut compared = 0usize;
        let mut lowest_similarity: f64 = 1.0;
        let mut unmeasured_text = 0usize;
        let mut compared_pages: Vec<(String, String)> = Vec::new();
        for url in &pages {
            let Some(file) = Self::answer(input.out, &url.path, &redirects) else { continue };
            let new_html =
                tokio::fs::read_to_string(input.out.join(&file)).await.unwrap_or_default();
            let base = Url::parse("https://new.invalid/").expect("static URL");
            let new_meta = super::html::analyse(&new_html, &base.join(&url.path).unwrap_or(base));
            let new_canonical = new_meta.meta.canonical.as_deref().and_then(|c| {
                if c.starts_with('/') {
                    Some(c.to_string())
                } else {
                    Url::parse(c).ok().map(|u| u.path().to_string())
                }
            });
            compared_pages
                .push((url.path.clone(), format!("/{}", file.trim_end_matches("index.html"))));
            let Some(legacy) = self.legacy_meta(url).await else {
                unmeasured_text += 1;
                continue;
            };
            compared += 1;
            let similarity = text_similarity(&legacy.main_text, &main_text(&new_html));
            lowest_similarity = lowest_similarity.min(similarity);
            if similarity < self.config.min_text_similarity {
                finding(
                    "text-similarity",
                    &url.path,
                    format!(
                        "main-text similarity {similarity:.3} is under {}",
                        self.config.min_text_similarity
                    ),
                );
            }
            if let Some(title) = &legacy.title
                && new_meta.meta.title.as_deref().map(title_core) != Some(title_core(title))
            {
                finding(
                    "metadata-title",
                    &url.path,
                    format!(
                        "title '{title}' became '{}'",
                        new_meta.meta.title.clone().unwrap_or_default()
                    ),
                );
            }
            if let Some(canonical) = &legacy.canonical
                && canonical.starts_with('/')
                && new_canonical.as_deref() != Some(canonical.as_str())
                && !redirects.iter().any(|r| &r.from == canonical)
            {
                finding(
                    "metadata-canonical",
                    &url.path,
                    format!(
                        "canonical {canonical} became {}",
                        new_meta.meta.canonical.clone().unwrap_or_default()
                    ),
                );
            }
            if legacy.description.is_some() && new_meta.meta.description.is_none() {
                finding(
                    "metadata-description",
                    &url.path,
                    "the page lost its meta description".into(),
                );
            }
            let primary = |l: &str| l.split('-').next().unwrap_or(l).to_ascii_lowercase();
            if let (Some(old), Some(new)) = (&legacy.language, &new_meta.meta.language)
                && primary(old) != primary(new)
            {
                finding("metadata-language", &url.path, format!("language {old} became {new}"));
            }
        }
        let mut largest_difference: f64 = 0.0;
        let mut visual_note = None;
        match (&self.screenshotter, &self.legacy) {
            (Some(screenshotter), Some(legacy)) => {
                let server;
                let preview = match &self.preview {
                    Some(preview) => preview.clone(),
                    None => {
                        server =
                            StaticServer::serve(input.out).await.map_err(crate::SiteError::from)?;
                        Url::parse(&server.origin())
                            .map_err(|e| crate::SiteError::GateFailed(e.to_string()))?
                    }
                };
                for (old_path, new_path) in &compared_pages {
                    for width in &self.config.widths {
                        let (Ok(old), Ok(new)) = (legacy.join(old_path), preview.join(new_path))
                        else {
                            continue;
                        };
                        let shots = (
                            screenshotter.screenshot(&old, *width, self.config.height).await,
                            screenshotter.screenshot(&new, *width, self.config.height).await,
                        );
                        let (old_shot, new_shot) = match shots {
                            (Ok(a), Ok(b)) => (a, b),
                            (Err(e), _) | (_, Err(e)) => {
                                finding(
                                    "perceptual-difference",
                                    old_path,
                                    format!("could not compare at {width}px: {e}"),
                                );
                                continue;
                            }
                        };
                        let difference = perceptual_difference(&old_shot, &new_shot)
                            .map_err(crate::SiteError::from)?;
                        largest_difference = largest_difference.max(difference);
                        if difference >= self.config.max_perceptual_difference {
                            finding(
                                "perceptual-difference",
                                old_path,
                                format!(
                                    "perceptual difference {difference:.3} at {width}px is not under {}",
                                    self.config.max_perceptual_difference
                                ),
                            );
                        }
                    }
                }
            }
            _ => {
                visual_note = Some(
                    "visual comparison not measured: no screenshotter or legacy view configured"
                        .to_string(),
                )
            }
        }
        let failed = findings.iter().any(|f| f.blocking);
        let design = if gates_looks {
            format!(
                "design kept: the perceptual difference gates the launch under {}",
                self.config.max_perceptual_difference
            )
        } else {
            "new theme: the perceptual difference is reported, not gated; the owner approves the look from the preview".to_string()
        };
        let mut note = format!(
            "{design}; {covered} URLs covered; {compared} pages compared, lowest text similarity {lowest_similarity:.3}, largest perceptual difference {largest_difference:.3}"
        );
        if unmeasured_text > 0 {
            note.push_str(&format!("; {unmeasured_text} pages had no legacy text to compare"));
        }
        if let Some(visual) = &visual_note {
            note.push_str("; ");
            note.push_str(visual);
        }
        tracing::info!(covered, compared, failed, design = %self.config.design, "migration parity checked");
        // An unmeasured visual comparison leaves the check unmeasured only when looks gate it.
        let unmeasured = (visual_note.is_some() && gates_looks) || unmeasured_text > 0;
        Ok(CheckResult {
            check: GateCheck::MigrationParity,
            status: if failed {
                CheckStatus::Failed
            } else if unmeasured {
                CheckStatus::NotMeasured
            } else {
                CheckStatus::Passed
            },
            findings,
            note: Some(note),
        })
    }
}
