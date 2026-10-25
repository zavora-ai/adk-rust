//! Site generators and the built-in renderer (spec 3.5 §7.8 "Generator", §7.4 "Machine view
//! of every page", §7.8.2 step 5).
//!
//! [`BuiltinGenerator`] renders Markdown and templates with no plugins. For every page it
//! writes `index.html`, the Markdown twin `index.md` and the structured twin
//! `index.awp.json` (an awp-types [`MachineView`](awp_types::MachineView)), linked from the
//! HTML with `<link rel="alternate">`; and for the site `llms.txt`, `robots.txt` with
//! `Content-Usage` from the charter's public terms, `/.well-known/security.txt`, sitemaps
//! with `lastmod`, Atom feeds, `/.well-known/awp/charter.json`, schema.org JSON-LD per content
//! kind, and redirects in the host's format.
//!
//! Output is deterministic: identical projects give byte-identical files. Incremental builds
//! render the changed items and their dependents only, from the [`DependencyGraph`] of the
//! previous build, and give the same files a full build gives.

#[cfg(any(
    feature = "gen-zola",
    feature = "gen-hugo",
    feature = "gen-astro",
    feature = "gen-build-awesome"
))]
pub mod external;
mod files;
mod plan;
mod render;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use awp_types::{ContentId, GeneratorRef, LanguageTag, Redirect};
use serde::{Deserialize, Serialize};

use crate::content::SiteProject;
use crate::error::{Result, SiteError};

pub use files::CHARTER_PATH;
pub use plan::DepKey;
pub use render::markdown_to_html;

/// Builds a site's files from a project (spec 3.5 §7.8).
///
/// External generators (`gen-zola`, `gen-hugo`, …) implement it as sandboxed processes;
/// [`BuiltinGenerator`] is the default.
#[async_trait]
pub trait SiteGenerator: Send + Sync {
    /// Builds every file of `project` into `out`, leaving nothing else there.
    ///
    /// # Errors
    ///
    /// Returns an error when the project is invalid, a template fails or `out` cannot be
    /// written.
    async fn build(&self, project: &SiteProject, out: &Path) -> Result<BuildReport>;

    /// Rebuilds in `out`, which holds the build `changes.previous` describes, only what the
    /// changed items affect. Generators without dependency tracking build everything.
    ///
    /// # Errors
    ///
    /// As [`build`](Self::build).
    async fn build_incremental(
        &self,
        project: &SiteProject,
        out: &Path,
        changes: &ChangeSet,
    ) -> Result<BuildReport> {
        let _ = changes;
        self.build(project, out).await
    }
}

/// What changed since the previous build.
#[derive(Debug, Clone, Default)]
pub struct ChangeSet {
    /// The items added, edited, removed, scheduled or withdrawn.
    pub items: BTreeSet<ContentId>,
    /// The previous build's dependency graph.
    pub previous: DependencyGraph,
}

/// Which outputs depend on what, and what each item touches (spec 3.5 §7.8.2 step 5).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DependencyGraph {
    /// Output path → the keys it depends on.
    pub outputs: BTreeMap<String, BTreeSet<DepKey>>,
    /// Item → the keys it touches.
    pub footprints: BTreeMap<ContentId, BTreeSet<DepKey>>,
    /// Language → navigation signature.
    pub nav: BTreeMap<String, String>,
    /// Translation group (`<kind>/<slug>`) → alternates signature.
    pub groups: BTreeMap<String, String>,
}

impl DependencyGraph {
    /// The keys a change to `items` touches between this (previous) graph and `next`.
    pub fn affected_keys(
        &self,
        next: &DependencyGraph,
        items: &BTreeSet<ContentId>,
    ) -> BTreeSet<DepKey> {
        let mut keys = BTreeSet::new();
        for id in items {
            keys.insert(DepKey::Item { id: id.clone() });
            for graph in [self, next] {
                if let Some(footprint) = graph.footprints.get(id) {
                    keys.extend(footprint.iter().cloned());
                }
            }
        }
        let languages: BTreeSet<&String> = self.nav.keys().chain(next.nav.keys()).collect();
        for language in languages {
            if self.nav.get(language) != next.nav.get(language) {
                keys.insert(DepKey::Nav { language: language.clone() });
            }
        }
        let groups: BTreeSet<&String> = self.groups.keys().chain(next.groups.keys()).collect();
        for group in groups {
            if self.groups.get(group) != next.groups.get(group)
                && let Some((kind, slug)) = group.split_once('/')
                && let Ok(kind) = serde_json::from_value(serde_json::Value::String(kind.into()))
            {
                keys.insert(DepKey::Group { kind, slug: slug.to_string() });
            }
        }
        keys
    }

    /// The outputs whose dependencies meet `keys`.
    pub fn outputs_for(&self, keys: &BTreeSet<DepKey>) -> BTreeSet<String> {
        self.outputs
            .iter()
            .filter(|(_, deps)| !deps.is_disjoint(keys))
            .map(|(path, _)| path.clone())
            .collect()
    }
}

/// One page of a build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageRecord {
    /// The page's URL path.
    pub path: String,
    /// The page's language.
    pub language: LanguageTag,
    /// The item the page shows, if one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ContentId>,
    /// The HTML file.
    pub html: String,
    /// The Markdown twin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
    /// The machine view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_view: Option<String>,
}

/// What a build produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildReport {
    /// The generator, for the release manifest.
    pub generator: GeneratorRef,
    /// Every page, sorted by path.
    pub pages: Vec<PageRecord>,
    /// Every file the build owns in `out`.
    pub outputs: BTreeSet<String>,
    /// Files this build wrote.
    pub written: BTreeSet<String>,
    /// Files this build removed.
    pub removed: BTreeSet<String>,
    /// The full redirect set, 410 rules for withdrawn pages included.
    pub redirects: Vec<Redirect>,
    /// The dependency graph for the next incremental build.
    pub graph: DependencyGraph,
    /// The theme, for the accessibility check of its tokens.
    pub theme: String,
    /// Whether the build was incremental.
    pub incremental: bool,
}

/// The built-in renderer: Markdown plus templates, no plugins (spec 3.5 §7.8).
///
/// # Example
///
/// ```rust
/// use adk_site::content::{SiteConfig, SiteProject};
/// use adk_site::generator::{BuiltinGenerator, SiteGenerator};
/// use chrono::{TimeZone, Utc};
///
/// # async fn demo() -> adk_site::Result<()> {
/// let config = SiteConfig::new("Example", "example.com", vec!["en".parse().unwrap()]);
/// let project = SiteProject::new(config, Vec::new(), Utc.with_ymd_and_hms(2026, 10, 25, 0, 0, 0).unwrap());
/// let out = tempfile::tempdir().unwrap();
/// let report = BuiltinGenerator.build(&project, out.path()).await?;
/// assert!(report.outputs.contains("index.html"));
/// assert!(report.outputs.contains("llms.txt"));
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct BuiltinGenerator;

impl BuiltinGenerator {
    /// The generator's reference in release manifests.
    pub fn reference() -> GeneratorRef {
        GeneratorRef { name: "adk-site".into(), version: env!("CARGO_PKG_VERSION").into() }
    }
}

#[async_trait]
impl SiteGenerator for BuiltinGenerator {
    async fn build(&self, project: &SiteProject, out: &Path) -> Result<BuildReport> {
        let project = Arc::new(project.clone());
        let out = out.to_path_buf();
        tokio::task::spawn_blocking(move || build_sync(&project, &out, None))
            .await
            .map_err(|e| SiteError::Pipeline(format!("build task failed: {e}")))?
    }

    async fn build_incremental(
        &self,
        project: &SiteProject,
        out: &Path,
        changes: &ChangeSet,
    ) -> Result<BuildReport> {
        let project = Arc::new(project.clone());
        let out = out.to_path_buf();
        let changes = changes.clone();
        tokio::task::spawn_blocking(move || build_sync(&project, &out, Some(&changes)))
            .await
            .map_err(|e| SiteError::Pipeline(format!("build task failed: {e}")))?
    }
}

enum Output {
    Page(usize),
    Media(String),
    Site,
}

fn write(out: &Path, path: &str, bytes: &[u8]) -> Result<()> {
    let full = out.join(path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SiteError::io(parent, e))?;
    }
    std::fs::write(&full, bytes).map_err(|e| SiteError::io(&full, e))
}

/// Every file under `dir`, as `/`-separated relative paths.
pub(crate) fn list_files(dir: &Path) -> Result<BTreeSet<String>> {
    let mut files = BTreeSet::new();
    if !dir.exists() {
        return Ok(files);
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current).map_err(|e| SiteError::io(&current, e))? {
            let entry = entry.map_err(|e| SiteError::io(&current, e))?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|e| SiteError::io(&path, e))?;
            if file_type.is_dir() {
                stack.push(path);
            } else {
                let relative =
                    path.strip_prefix(dir).unwrap_or(&path).to_string_lossy().replace('\\', "/");
                files.insert(relative);
            }
        }
    }
    Ok(files)
}

fn remove(out: &Path, path: &str) -> Result<()> {
    let full = out.join(path);
    match std::fs::remove_file(&full) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(SiteError::io(&full, e)),
    }
    let mut dir = full.parent();
    while let Some(current) = dir {
        if current == out || std::fs::remove_dir(current).is_err() {
            break;
        }
        dir = current.parent();
    }
    Ok(())
}

fn build_sync(
    project: &SiteProject,
    out: &Path,
    changes: Option<&ChangeSet>,
) -> Result<BuildReport> {
    if let Some(issue) = project.issues().into_iter().next() {
        return Err(SiteError::InvalidContent {
            path: issue.item.map_or_else(|| "site.toml".into(), |id| id.0),
            message: issue.message,
        });
    }
    let plan = plan::plan(project)?;
    let mut outputs: BTreeMap<String, (Output, BTreeSet<DepKey>)> = BTreeMap::new();
    let mut claim = |path: String, output: Output, deps: BTreeSet<DepKey>| -> Result<()> {
        if outputs.insert(path.clone(), (output, deps)).is_some() {
            return Err(SiteError::InvalidContent {
                path,
                message: "two outputs share this file; rename a media file or change a redirect"
                    .into(),
            });
        }
        Ok(())
    };
    for (index, page) in plan.pages.iter().enumerate() {
        let (html, md, view) = plan::page_files(&page.path);
        for path in std::iter::once(html).chain(md).chain(view) {
            claim(path, Output::Page(index), page.deps.clone())?;
        }
    }
    for (path, repo_path, index) in &plan.media {
        let deps = BTreeSet::from([DepKey::Item { id: project.items[*index].id.clone() }]);
        claim(path.clone(), Output::Media(repo_path.clone()), deps)?;
    }
    for (path, deps) in files::site_file_paths(project, &plan) {
        claim(path, Output::Site, deps)?;
    }
    let graph = DependencyGraph {
        outputs: outputs.iter().map(|(path, (_, deps))| (path.clone(), deps.clone())).collect(),
        footprints: plan.footprints.clone(),
        nav: project
            .config
            .languages
            .iter()
            .map(|l| (plan::lang_key(l), plan.nav_signature(&plan::lang_key(l))))
            .collect(),
        groups: plan.group_signatures(&project.items),
    };
    let on_disk = list_files(out)?;
    let (render, removed): (BTreeSet<String>, BTreeSet<String>) = match changes {
        None => {
            let stale: BTreeSet<String> =
                on_disk.difference(&graph.outputs.keys().cloned().collect()).cloned().collect();
            (graph.outputs.keys().cloned().collect(), stale)
        }
        Some(changes) => {
            let keys = changes.previous.affected_keys(&graph, &changes.items);
            let mut render = graph.outputs_for(&keys);
            for path in graph.outputs.keys() {
                if !changes.previous.outputs.contains_key(path) || !on_disk.contains(path) {
                    render.insert(path.clone());
                }
            }
            let removed = changes
                .previous
                .outputs
                .keys()
                .filter(|path| !graph.outputs.contains_key(*path))
                .cloned()
                .collect();
            (render, removed)
        }
    };
    for path in &removed {
        remove(out, path)?;
    }
    let renderer = render::Renderer::new(project, &plan)?;
    let mut written = BTreeSet::new();
    let mut rendered_pages = BTreeSet::new();
    for path in &render {
        let Some((output, _)) = outputs.get(path) else { continue };
        match output {
            Output::Page(index) => {
                if !rendered_pages.insert(*index) {
                    continue;
                }
                for (file, bytes) in renderer.page(&plan.pages[*index])? {
                    write(out, &file, &bytes)?;
                    written.insert(file);
                }
            }
            Output::Media(repo_path) => {
                write(out, path, &project.media_bytes(repo_path)?)?;
                written.insert(path.clone());
            }
            Output::Site => {
                write(out, path, &files::render(project, &plan, path)?)?;
                written.insert(path.clone());
            }
        }
    }
    let pages = plan
        .pages
        .iter()
        .map(|page| {
            let (html, markdown, machine_view) = plan::page_files(&page.path);
            let item = match &page.kind {
                plan::PageKind::Item(i) | plan::PageKind::Notice(i, _) => {
                    Some(project.items[*i].id.clone())
                }
                plan::PageKind::Home { item, .. } => item.map(|i| project.items[i].id.clone()),
                plan::PageKind::Listing { collection, .. } => {
                    collection.map(|i| project.items[i].id.clone())
                }
                plan::PageKind::NotFound | plan::PageKind::Gone => None,
            };
            PageRecord {
                path: page.path.clone(),
                language: page.language.clone(),
                item,
                html,
                markdown,
                machine_view,
            }
        })
        .collect();
    Ok(BuildReport {
        generator: BuiltinGenerator::reference(),
        pages,
        outputs: graph.outputs.keys().cloned().collect(),
        written,
        removed,
        redirects: files::redirects(project, &plan),
        graph,
        theme: project.theme.name.clone(),
        incremental: changes.is_some(),
    })
}
