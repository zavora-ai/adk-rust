//! Content stores: where items and their media live (spec 3.5 §7.8.2 "Content model").

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use awp_types::{ContentId, ContentItem, LanguageTag};
use bytes::Bytes;
use tokio::sync::RwLock;

use super::frontmatter::{ItemDefaults, parse_item, render_item};
use super::{CONTENT_DIR, kind_directory, kind_from_directory};
use crate::error::{Result, SiteError};

/// Reads and writes a site's content items and the media beside them.
///
/// Drafts on branches and the repository's history are the caller's git concern; a store
/// holds one working copy. Implementations run [`content_store_conformance`].
#[async_trait]
pub trait ContentStore: Send + Sync {
    /// Every item, sorted by id.
    ///
    /// # Errors
    ///
    /// Returns an error when the store cannot be read or an item is malformed.
    async fn list(&self) -> Result<Vec<ContentItem>>;

    /// The item with `id`.
    ///
    /// # Errors
    ///
    /// Returns an error when the store cannot be read.
    async fn get(&self, id: &ContentId) -> Result<Option<ContentItem>>;

    /// Creates or replaces an item.
    ///
    /// # Errors
    ///
    /// Returns an error when the item cannot be written.
    async fn put(&self, item: &ContentItem) -> Result<()>;

    /// Removes an item; returns whether it existed. Its media stays.
    ///
    /// # Errors
    ///
    /// Returns an error when the store cannot be written.
    async fn remove(&self, id: &ContentId) -> Result<bool>;

    /// Reads a media file by its repository path.
    ///
    /// # Errors
    ///
    /// Returns an error for a path outside the repository or a failing store.
    async fn read_media(&self, path: &str) -> Result<Option<Bytes>>;

    /// Writes a media file at its repository path.
    ///
    /// # Errors
    ///
    /// Returns an error for a path outside the repository or a failing store.
    async fn write_media(&self, path: &str, bytes: Bytes) -> Result<()>;
}

/// Checks that a repository path stays inside the repository.
pub(crate) fn safe_relative(path: &str) -> Result<PathBuf> {
    let candidate = Path::new(path);
    let ok = !path.is_empty()
        && candidate.components().all(|c| matches!(c, Component::Normal(_)))
        && !path.contains('\\');
    if ok {
        Ok(candidate.to_path_buf())
    } else {
        Err(SiteError::InvalidContent {
            path: path.to_string(),
            message: "media paths are relative to the repository and may not climb out of it"
                .into(),
        })
    }
}

/// A store in memory, for tests and generated sites.
///
/// # Example
///
/// ```rust
/// use adk_site::content::{ContentStore, InMemoryContentStore};
///
/// # async fn demo() -> adk_site::Result<()> {
/// let store = InMemoryContentStore::default();
/// assert!(store.list().await?.is_empty());
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default, Clone)]
pub struct InMemoryContentStore {
    items: Arc<RwLock<BTreeMap<ContentId, ContentItem>>>,
    media: Arc<RwLock<BTreeMap<String, Bytes>>>,
}

#[async_trait]
impl ContentStore for InMemoryContentStore {
    async fn list(&self) -> Result<Vec<ContentItem>> {
        Ok(self.items.read().await.values().cloned().collect())
    }

    async fn get(&self, id: &ContentId) -> Result<Option<ContentItem>> {
        Ok(self.items.read().await.get(id).cloned())
    }

    async fn put(&self, item: &ContentItem) -> Result<()> {
        self.items.write().await.insert(item.id.clone(), item.clone());
        Ok(())
    }

    async fn remove(&self, id: &ContentId) -> Result<bool> {
        Ok(self.items.write().await.remove(id).is_some())
    }

    async fn read_media(&self, path: &str) -> Result<Option<Bytes>> {
        safe_relative(path)?;
        Ok(self.media.read().await.get(path).cloned())
    }

    async fn write_media(&self, path: &str, bytes: Bytes) -> Result<()> {
        safe_relative(path)?;
        self.media.write().await.insert(path.to_string(), bytes);
        Ok(())
    }
}

/// The content repository on disk: `content/<kind>/<slug>/<language>.md` with media beside
/// each item (spec 3.5 §7.8.2).
///
/// # Example
///
/// ```rust,no_run
/// use adk_site::content::{ContentStore, FsContentStore};
///
/// # async fn demo() -> adk_site::Result<()> {
/// let store = FsContentStore::new("./my-site");
/// for item in store.list().await? {
///     println!("{}", item.id);
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct FsContentStore {
    root: PathBuf,
    lock: Arc<RwLock<()>>,
}

impl FsContentStore {
    /// A store over the repository at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), lock: Arc::new(RwLock::new(())) }
    }

    /// The repository root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The file an item is stored in, relative to the repository root.
    pub fn item_path(item: &ContentItem) -> String {
        format!("{CONTENT_DIR}/{}/{}/{}.md", kind_directory(item.kind), item.slug, item.language)
    }

    fn scan(root: &Path) -> Result<Vec<(String, ContentItem)>> {
        let content = root.join(CONTENT_DIR);
        let mut found = Vec::new();
        if !content.exists() {
            return Ok(found);
        }
        let mut stack = vec![content.clone()];
        while let Some(dir) = stack.pop() {
            let entries = std::fs::read_dir(&dir).map_err(|e| SiteError::io(&dir, e))?;
            for entry in entries {
                let entry = entry.map_err(|e| SiteError::io(&dir, e))?;
                let path = entry.path();
                let file_type = entry.file_type().map_err(|e| SiteError::io(&path, e))?;
                if file_type.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("md") || !file_type.is_file() {
                    continue;
                }
                let relative = path
                    .strip_prefix(&content)
                    .map_err(|_| SiteError::InvalidContent {
                        path: path.display().to_string(),
                        message: "outside content/".into(),
                    })?
                    .to_string_lossy()
                    .replace('\\', "/");
                let origin = format!("{CONTENT_DIR}/{relative}");
                let mut parts: Vec<&str> = relative.split('/').collect();
                let file = parts.pop().unwrap_or_default();
                let invalid = |message: &str| SiteError::InvalidContent {
                    path: origin.clone(),
                    message: message.into(),
                };
                if parts.len() < 2 {
                    return Err(invalid("items live at content/<kind>/<slug>/<language>.md"));
                }
                let kind = kind_from_directory(parts[0])
                    .ok_or_else(|| invalid("unknown kind directory"))?;
                let slug = parts[1..].join("/");
                let language: LanguageTag = file
                    .trim_end_matches(".md")
                    .parse()
                    .map_err(|_| invalid("the file name must be a language tag, such as en.md"))?;
                let text = std::fs::read_to_string(&path).map_err(|e| SiteError::io(&path, e))?;
                let id = ContentId(format!("{}/{slug}/{language}", parts[0]));
                let item = parse_item(&origin, &text, ItemDefaults { id, kind, slug, language })?;
                found.push((origin, item));
            }
        }
        found.sort_by(|a, b| a.1.id.cmp(&b.1.id));
        Ok(found)
    }

    async fn blocking<T: Send + 'static>(
        &self,
        job: impl FnOnce(&Path) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || job(&root))
            .await
            .map_err(|e| SiteError::Pipeline(format!("content store task failed: {e}")))?
    }
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SiteError::io(parent, e))?;
    }
    let tmp = path.with_extension(format!(
        "{}.tmp-{}",
        path.extension().and_then(|e| e.to_str()).unwrap_or(""),
        std::process::id()
    ));
    std::fs::write(&tmp, bytes).map_err(|e| SiteError::io(&tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| SiteError::io(path, e))
}

#[async_trait]
impl ContentStore for FsContentStore {
    async fn list(&self) -> Result<Vec<ContentItem>> {
        let _guard = self.lock.read().await;
        Ok(self.blocking(Self::scan).await?.into_iter().map(|(_, item)| item).collect())
    }

    async fn get(&self, id: &ContentId) -> Result<Option<ContentItem>> {
        Ok(self.list().await?.into_iter().find(|item| &item.id == id))
    }

    async fn put(&self, item: &ContentItem) -> Result<()> {
        let _guard = self.lock.write().await;
        let target = Self::item_path(item);
        safe_relative(&target)?;
        let text = render_item(item)?;
        let id = item.id.clone();
        self.blocking(move |root| {
            for (origin, existing) in Self::scan(root)? {
                if existing.id == id && origin != target {
                    let old = root.join(&origin);
                    std::fs::remove_file(&old).map_err(|e| SiteError::io(&old, e))?;
                }
            }
            write_atomic(&root.join(&target), text.as_bytes())
        })
        .await
    }

    async fn remove(&self, id: &ContentId) -> Result<bool> {
        let _guard = self.lock.write().await;
        let id = id.clone();
        self.blocking(move |root| {
            let mut removed = false;
            for (origin, existing) in Self::scan(root)? {
                if existing.id == id {
                    let path = root.join(&origin);
                    std::fs::remove_file(&path).map_err(|e| SiteError::io(&path, e))?;
                    removed = true;
                }
            }
            Ok(removed)
        })
        .await
    }

    async fn read_media(&self, path: &str) -> Result<Option<Bytes>> {
        let full = self.root.join(safe_relative(path)?);
        match tokio::fs::read(&full).await {
            Ok(bytes) => Ok(Some(Bytes::from(bytes))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(SiteError::io(full, e)),
        }
    }

    async fn write_media(&self, path: &str, bytes: Bytes) -> Result<()> {
        let full = self.root.join(safe_relative(path)?);
        self.blocking(move |_| write_atomic(&full, &bytes)).await
    }
}

/// The conformance suite every [`ContentStore`] runs: put, get, list order, replace,
/// remove, media round trip and path confinement.
///
/// # Panics
///
/// Panics with the failed expectation.
pub async fn content_store_conformance(store: &dyn ContentStore, sample: ContentItem) {
    assert!(store.get(&sample.id).await.unwrap().is_none(), "store must start empty");
    store.put(&sample).await.unwrap();
    assert_eq!(store.get(&sample.id).await.unwrap().as_ref(), Some(&sample), "get after put");

    let mut second = sample.clone();
    second.id = ContentId(format!("{}-b", sample.id.0));
    second.slug = format!("{}-b", sample.slug);
    store.put(&second).await.unwrap();
    let ids: Vec<ContentId> = store.list().await.unwrap().into_iter().map(|i| i.id).collect();
    assert_eq!(ids, vec![sample.id.clone(), second.id.clone()], "list is sorted by id");

    let mut changed = sample.clone();
    changed.fields.insert("title".into(), "Changed".into());
    store.put(&changed).await.unwrap();
    assert_eq!(store.get(&sample.id).await.unwrap(), Some(changed), "put replaces");

    assert!(store.remove(&second.id).await.unwrap());
    assert!(!store.remove(&second.id).await.unwrap());
    assert_eq!(store.list().await.unwrap().len(), 1, "remove deletes one item");

    store.write_media("content/media/a.png", Bytes::from_static(b"png")).await.unwrap();
    assert_eq!(
        store.read_media("content/media/a.png").await.unwrap(),
        Some(Bytes::from_static(b"png"))
    );
    assert_eq!(store.read_media("content/media/none.png").await.unwrap(), None);
    assert!(store.write_media("../escape.png", Bytes::new()).await.is_err(), "no climbing out");
    assert!(store.read_media("/etc/passwd").await.is_err(), "no absolute paths");
}
