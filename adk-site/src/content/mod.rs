//! The content model and repository (spec 3.5 §7.8.2 "Content model").
//!
//! Content lives as files in the site's content repository:
//!
//! | Path | Holds |
//! |------|-------|
//! | `site.toml` | [`SiteConfig`] |
//! | `content/<kind>/<slug>/<language>.md` | One [`ContentItem`] per language: front matter, then a Markdown body |
//! | `content/<kind>/<slug>/*` | The item's media, beside it |
//! | `public-terms.json` | The charter's [`PublicTerms`], written by the charter store |
//! | `capabilities.json` | The site's [`CapabilityV2`] declarations, for machine views |
//! | `theme/` | An optional [`Theme`] directory |
//!
//! Kind directories are `pages`, `posts`, `articles`, `products`, `events`, `notices`,
//! `services`, `people` and `collections`. Drafts on branches are the caller's git concern.

mod config;
mod frontmatter;
mod history;
mod store;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use awp_types::{CapabilityV2, ContentId, ContentItem, ContentKind, PublicTerms};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Result, SiteError};
use crate::redirects::RedirectsFormat;
use crate::theme::Theme;

pub use config::{PerformanceBudget, SecurityContact, SiteConfig};
pub use history::{
    ContentHistory, FileContentHistory, HistoryEntry, HistoryMeta, InMemoryContentHistory,
    content_history_conformance,
};
pub use store::{ContentStore, FsContentStore, InMemoryContentStore, content_store_conformance};

/// The directory content items live under.
pub const CONTENT_DIR: &str = "content";

/// The repository directory of a content kind.
///
/// # Example
///
/// ```rust
/// use adk_site::content::kind_directory;
/// use awp_types::ContentKind;
///
/// assert_eq!(kind_directory(ContentKind::Person), "people");
/// ```
pub fn kind_directory(kind: ContentKind) -> &'static str {
    match kind {
        ContentKind::Page => "pages",
        ContentKind::Post => "posts",
        ContentKind::Article => "articles",
        ContentKind::Product => "products",
        ContentKind::Event => "events",
        ContentKind::Notice => "notices",
        ContentKind::Service => "services",
        ContentKind::Person => "people",
        ContentKind::Collection => "collections",
        _ => "items",
    }
}

/// The content kind of a repository directory.
pub fn kind_from_directory(dir: &str) -> Option<ContentKind> {
    Some(match dir {
        "pages" => ContentKind::Page,
        "posts" => ContentKind::Post,
        "articles" => ContentKind::Article,
        "products" => ContentKind::Product,
        "events" => ContentKind::Event,
        "notices" => ContentKind::Notice,
        "services" => ContentKind::Service,
        "people" => ContentKind::Person,
        "collections" => ContentKind::Collection,
        _ => return None,
    })
}

/// The wire name of a content kind, such as `product`.
pub fn kind_name(kind: ContentKind) -> String {
    serde_json::to_value(kind).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

/// A problem with a project that stops a build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectIssue {
    /// The item, when the issue is an item's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ContentId>,
    /// What is wrong.
    pub message: String,
}

/// Everything a generator needs to build a site: configuration, items, media, theme,
/// public terms and capabilities, at a point in time (spec 3.5 §7.8).
///
/// Identical projects build identical files: `as_of` stands in for the clock, and nothing
/// else in the build reads the time.
///
/// # Example
///
/// ```rust
/// use adk_site::content::{SiteConfig, SiteProject};
/// use chrono::{TimeZone, Utc};
///
/// let config = SiteConfig::new("Example", "example.com", vec!["en".parse().unwrap()]);
/// let project = SiteProject::new(config, Vec::new(), Utc.with_ymd_and_hms(2026, 10, 25, 0, 0, 0).unwrap());
/// assert!(project.issues().is_empty());
/// ```
#[derive(Debug, Clone)]
pub struct SiteProject {
    /// The site configuration.
    pub config: SiteConfig,
    /// Every item, sorted by id.
    pub items: Vec<ContentItem>,
    /// Media by repository path, for items not read from disk.
    pub media: BTreeMap<String, Bytes>,
    /// The repository root, where media not in `media` is read from.
    pub root: Option<PathBuf>,
    /// The theme.
    pub theme: Theme,
    /// The charter's public terms.
    pub public_terms: Option<PublicTerms>,
    /// The site's capabilities, listed in machine views.
    pub capabilities: Vec<CapabilityV2>,
    /// The build time: schedules are evaluated at it and `security.txt` expires a year after.
    pub as_of: DateTime<Utc>,
    /// The release the build is for, stamped on machine views.
    pub release_seq: u64,
    /// The redirect format of the host the release goes to.
    pub redirects_format: RedirectsFormat,
    /// Whether to render drafts, items in review and future schedules: a preview build.
    pub preview: bool,
    /// Documents other components add to the release by path, such as the signed contract
    /// `.well-known/awp.json`, `.well-known/awp/keys.json` and an A2A Agent Card (AWP 2.0
    /// §14.2). Pages link the contract with `<link rel="awp">` when it is here.
    pub documents: BTreeMap<String, Bytes>,
}

impl SiteProject {
    /// A project from parts, with the default theme.
    pub fn new(config: SiteConfig, mut items: Vec<ContentItem>, as_of: DateTime<Utc>) -> Self {
        items.sort_by(|a, b| a.id.cmp(&b.id));
        Self {
            config,
            items,
            media: BTreeMap::new(),
            root: None,
            theme: Theme::default(),
            public_terms: None,
            capabilities: Vec::new(),
            as_of,
            release_seq: 1,
            redirects_format: RedirectsFormat::default(),
            preview: false,
            documents: BTreeMap::new(),
        }
    }

    /// Loads a content repository from disk.
    ///
    /// # Errors
    ///
    /// [`SiteError::InvalidConfig`] for a missing or invalid `site.toml`; content, theme
    /// and JSON errors for the other files.
    pub async fn load(root: &Path, as_of: DateTime<Utc>) -> Result<Self> {
        let config_path = root.join("site.toml");
        let text = tokio::fs::read_to_string(&config_path)
            .await
            .map_err(|e| SiteError::io(&config_path, e))?;
        let config = SiteConfig::from_toml(&text)?;
        let items = FsContentStore::new(root).list().await?;
        let mut project = Self::new(config, items, as_of);
        project.root = Some(root.to_path_buf());
        let root_owned = root.to_path_buf();
        let theme_dir = root.join("theme");
        if theme_dir.is_dir() {
            project.theme = tokio::task::spawn_blocking(move || Theme::load(&theme_dir))
                .await
                .map_err(|e| SiteError::Pipeline(e.to_string()))??;
        }
        project.public_terms = read_json(&root_owned.join("public-terms.json")).await?;
        project.capabilities =
            read_json(&root_owned.join("capabilities.json")).await?.unwrap_or_default();
        Ok(project)
    }

    /// Builds a project from a store, reading every item's media into memory.
    ///
    /// # Errors
    ///
    /// Store errors; [`SiteError::InvalidContent`] when an item's media is missing.
    pub async fn from_store(
        config: SiteConfig,
        store: &dyn ContentStore,
        as_of: DateTime<Utc>,
    ) -> Result<Self> {
        let items = store.list().await?;
        let mut media = BTreeMap::new();
        for item in &items {
            for reference in &item.media {
                let bytes = store.read_media(&reference.path).await?.ok_or_else(|| {
                    SiteError::InvalidContent {
                        path: item.id.to_string(),
                        message: format!("media '{}' is missing from the store", reference.path),
                    }
                })?;
                media.insert(reference.path.clone(), bytes);
            }
        }
        let mut project = Self::new(config, items, as_of);
        project.media = media;
        Ok(project)
    }

    /// The item with `id`.
    pub fn item(&self, id: &ContentId) -> Option<&ContentItem> {
        self.items.iter().find(|item| &item.id == id)
    }

    /// The bytes of a media file.
    ///
    /// # Errors
    ///
    /// [`SiteError::InvalidContent`] when the file is in neither `media` nor the repository.
    pub fn media_bytes(&self, path: &str) -> Result<Bytes> {
        if let Some(bytes) = self.media.get(path) {
            return Ok(bytes.clone());
        }
        let missing = || SiteError::InvalidContent {
            path: path.to_string(),
            message: "media file is missing".into(),
        };
        let root = self.root.as_ref().ok_or_else(missing)?;
        let full = root.join(store::safe_relative(path)?);
        std::fs::read(&full).map(Bytes::from).map_err(|_| missing())
    }

    /// Structural problems that stop a build: unknown languages, duplicate ids, invalid
    /// slugs and missing media.
    pub fn issues(&self) -> Vec<ProjectIssue> {
        let mut issues = Vec::new();
        if let Err(error) = self.config.validate() {
            issues.push(ProjectIssue { item: None, message: error.to_string() });
        }
        let languages: BTreeSet<String> =
            self.config.languages.iter().map(|l| l.as_str().to_ascii_lowercase()).collect();
        let mut ids = BTreeSet::new();
        for item in &self.items {
            let mut issue = |message: String| {
                issues.push(ProjectIssue { item: Some(item.id.clone()), message })
            };
            if !ids.insert(item.id.clone()) {
                issue("the id is used by another item".into());
            }
            if !languages.contains(&item.language.as_str().to_ascii_lowercase()) {
                issue(format!("language '{}' is not one of the site's languages", item.language));
            }
            let slug_ok = item.slug.split('/').all(|segment| {
                !segment.is_empty()
                    && segment != "."
                    && segment != ".."
                    && !segment.chars().any(|c| c.is_whitespace() || "?#%\\<>\"".contains(c))
            });
            if !slug_ok {
                issue(format!("slug '{}' is not a valid URL path", item.slug));
            }
            if let Some(path) = item.fields.get("path") {
                let valid = path
                    .as_str()
                    .is_some_and(|p| p.starts_with('/') && p.ends_with('/') && !p.contains(".."));
                if !valid {
                    issue("field 'path' must be a URL path that starts and ends with '/'".into());
                }
            }
            for reference in &item.media {
                if self.media_bytes(&reference.path).is_err() {
                    issue(format!("media '{}' is missing", reference.path));
                }
            }
        }
        issues
    }
}

async fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match tokio::fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| SiteError::serde(path.display().to_string(), e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(SiteError::io(path, e)),
    }
}
