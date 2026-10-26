//! The back office's drafts, kept durable (spec 3.5 §7.8.2 step 1, §7.6, §7.11): what a
//! [`SiteRuntime`](super::SiteRuntime) holds between tool calls survives a restart.
//!
//! | Kind | What is kept |
//! |---|---|
//! | [`DraftKind::Content`] | A content draft: its items, its media, where it came from, its proposer, job and action class |
//! | [`DraftKind::Charter`] | A charter draft |
//! | [`DraftKind::CharterSign`] | A charter signing request and the signatures it has |
//! | [`DraftKind::Promotion`] | A request that the owner promote a job |
//!
//! A [`SiteDraftStore`] keeps each as JSON by site, kind and id: [`InMemorySiteDraftStore`]
//! for tests, [`SqliteSiteDraftStore`] (feature `sqlite`) for a served site. Every
//! implementation passes [`conformance::site_draft_store`]. A preview and the votes on it
//! are not kept: a preview's digest names the build it staged, so it is made again after a
//! restart.

use std::collections::BTreeMap;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use adk_site::pipeline::{Change, Draft, EntryPoint};
use async_trait::async_trait;
use awp_types::ContentItem;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;
use uuid::Uuid;

/// What a stored record is (spec 3.5 §7.8.2, §7.6, §7.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DraftKind {
    /// A content draft.
    Content,
    /// A charter draft.
    Charter,
    /// A charter signing request.
    CharterSign,
    /// A job promotion request.
    Promotion,
}

impl DraftKind {
    /// Every kind.
    pub const ALL: [DraftKind; 4] =
        [DraftKind::Content, DraftKind::Charter, DraftKind::CharterSign, DraftKind::Promotion];

    /// The stored name: `content`, `charter`, `charter_sign` or `promotion`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Content => "content",
            Self::Charter => "charter",
            Self::CharterSign => "charter_sign",
            Self::Promotion => "promotion",
        }
    }
}

/// Where a site runtime keeps its drafts (spec 3.5 §7.8.2 step 1).
///
/// # Example
///
/// ```rust
/// use adk_harness::website::{DraftKind, InMemorySiteDraftStore, SiteDraftStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_core::Result<()> {
/// let store = InMemorySiteDraftStore::default();
/// let site = uuid::Uuid::nil();
/// store.put(site, DraftKind::Charter, "charter-1", &serde_json::json!({ "v": 1 })).await?;
/// assert_eq!(store.list(site, DraftKind::Charter).await?.len(), 1);
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait SiteDraftStore: Send + Sync {
    /// Keeps `value` as `id` of `kind` for `site`, replacing an earlier one.
    ///
    /// # Errors
    ///
    /// The store's error.
    async fn put(&self, site: Uuid, kind: DraftKind, id: &str, value: &Value) -> Result<()>;

    /// Forgets `id` of `kind` for `site`; nothing when there is none.
    ///
    /// # Errors
    ///
    /// The store's error.
    async fn remove(&self, site: Uuid, kind: DraftKind, id: &str) -> Result<()>;

    /// Every record of `kind` for `site`, by id.
    ///
    /// # Errors
    ///
    /// The store's error.
    async fn list(&self, site: Uuid, kind: DraftKind) -> Result<Vec<(String, Value)>>;
}

/// A [`SiteDraftStore`] in memory, for tests.
#[derive(Debug, Default)]
pub struct InMemorySiteDraftStore {
    records: Mutex<BTreeMap<(Uuid, DraftKind, String), Value>>,
}

#[async_trait]
impl SiteDraftStore for InMemorySiteDraftStore {
    async fn put(&self, site: Uuid, kind: DraftKind, id: &str, value: &Value) -> Result<()> {
        self.records.lock().await.insert((site, kind, id.to_string()), value.clone());
        Ok(())
    }

    async fn remove(&self, site: Uuid, kind: DraftKind, id: &str) -> Result<()> {
        self.records.lock().await.remove(&(site, kind, id.to_string()));
        Ok(())
    }

    async fn list(&self, site: Uuid, kind: DraftKind) -> Result<Vec<(String, Value)>> {
        Ok(self
            .records
            .lock()
            .await
            .iter()
            .filter(|((s, k, _), _)| *s == site && *k == kind)
            .map(|((_, _, id), value)| (id.clone(), value.clone()))
            .collect())
    }
}

pub(crate) fn store_error(error: impl std::fmt::Display) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::Unavailable,
        "website.drafts.store",
        format!("the site's draft store: {error}"),
    )
}

/// A [`SiteDraftStore`] in SQLite, such as a served site's `data/edge.db` (feature
/// `sqlite`).
///
/// # Example
///
/// ```rust,ignore
/// let drafts = SqliteSiteDraftStore::new(pool);
/// drafts.migrate().await?;
/// let runtime = SiteRuntime::builder(site, base, store, generator, host, log, work)
///     .drafts(Arc::new(drafts))
///     .build();
/// runtime.restore_drafts().await?;
/// ```
#[cfg(feature = "sqlite")]
#[derive(Debug, Clone)]
pub struct SqliteSiteDraftStore {
    pool: sqlx::SqlitePool,
}

#[cfg(feature = "sqlite")]
impl SqliteSiteDraftStore {
    /// A store over `pool`; call [`migrate`](Self::migrate) once.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }

    /// Creates the table.
    ///
    /// # Errors
    ///
    /// The database's error.
    pub async fn migrate(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS adk_website_drafts (
                site TEXT NOT NULL,
                kind TEXT NOT NULL,
                id TEXT NOT NULL,
                value TEXT NOT NULL,
                PRIMARY KEY (site, kind, id)
            )",
        )
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(())
    }
}

#[cfg(feature = "sqlite")]
#[async_trait]
impl SiteDraftStore for SqliteSiteDraftStore {
    async fn put(&self, site: Uuid, kind: DraftKind, id: &str, value: &Value) -> Result<()> {
        sqlx::query(
            "INSERT INTO adk_website_drafts (site, kind, id, value) VALUES (?, ?, ?, ?)
             ON CONFLICT (site, kind, id) DO UPDATE SET value = excluded.value",
        )
        .bind(site.to_string())
        .bind(kind.as_str())
        .bind(id)
        .bind(value.to_string())
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn remove(&self, site: Uuid, kind: DraftKind, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM adk_website_drafts WHERE site = ? AND kind = ? AND id = ?")
            .bind(site.to_string())
            .bind(kind.as_str())
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(store_error)?;
        Ok(())
    }

    async fn list(&self, site: Uuid, kind: DraftKind) -> Result<Vec<(String, Value)>> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT id, value FROM adk_website_drafts WHERE site = ? AND kind = ? ORDER BY id",
        )
        .bind(site.to_string())
        .bind(kind.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        rows.into_iter()
            .map(|(id, value)| Ok((id, serde_json::from_str(&value).map_err(store_error)?)))
            .collect()
    }
}

/// A content draft as it is stored: the change, its media base64-encoded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoredDraft {
    items: Vec<ContentItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    media: Vec<(String, String)>,
    entry: EntryPoint,
    proposer: String,
    job: String,
    action_class: String,
}

impl StoredDraft {
    pub(crate) fn of(draft: &Draft) -> Self {
        let engine = base64::engine::general_purpose::STANDARD;
        Self {
            items: draft.change.items.clone(),
            media: draft
                .change
                .media
                .iter()
                .map(|(path, bytes)| (path.clone(), engine.encode(bytes)))
                .collect(),
            entry: draft.change.entry.clone(),
            proposer: draft.change.proposer.clone(),
            job: draft.change.job.clone(),
            action_class: draft.change.action_class.clone(),
        }
    }

    pub(crate) fn into_draft(self) -> Result<Draft> {
        let engine = base64::engine::general_purpose::STANDARD;
        let media = self
            .media
            .into_iter()
            .map(|(path, data)| {
                let bytes = engine.decode(data.as_bytes()).map_err(store_error)?;
                Ok((path, bytes::Bytes::from(bytes)))
            })
            .collect::<Result<Vec<_>>>()?;
        let ids = self.items.iter().map(|item| item.id.clone()).collect();
        Ok(Draft {
            change: Change {
                items: self.items,
                media,
                entry: self.entry,
                proposer: self.proposer,
                job: self.job,
                action_class: self.action_class,
            },
            ids,
        })
    }
}

/// The conformance check every [`SiteDraftStore`] passes (spec 3.5 §7.8.2).
pub mod conformance {
    use serde_json::json;
    use uuid::Uuid;

    use super::{DraftKind, SiteDraftStore};

    /// Puts, replaces, lists and removes records, kept apart by site and kind.
    ///
    /// # Panics
    ///
    /// When the store breaks its contract.
    pub async fn site_draft_store(store: &dyn SiteDraftStore) {
        let (site, other) = (Uuid::new_v4(), Uuid::new_v4());
        store.put(site, DraftKind::Content, "draft-2", &json!({ "n": 2 })).await.unwrap();
        store.put(site, DraftKind::Content, "draft-1", &json!({ "n": 1 })).await.unwrap();
        store.put(site, DraftKind::Charter, "charter-3", &json!({ "c": 3 })).await.unwrap();
        store.put(other, DraftKind::Content, "draft-1", &json!({ "other": true })).await.unwrap();
        assert_eq!(
            store.list(site, DraftKind::Content).await.unwrap(),
            vec![("draft-1".into(), json!({ "n": 1 })), ("draft-2".into(), json!({ "n": 2 }))],
            "listed by id, this site's and this kind's only"
        );
        store.put(site, DraftKind::Content, "draft-1", &json!({ "n": 10 })).await.unwrap();
        store.remove(site, DraftKind::Content, "draft-2").await.unwrap();
        store.remove(site, DraftKind::Content, "draft-9").await.unwrap();
        assert_eq!(
            store.list(site, DraftKind::Content).await.unwrap(),
            vec![("draft-1".into(), json!({ "n": 10 }))]
        );
        assert_eq!(store.list(site, DraftKind::Charter).await.unwrap().len(), 1);
        assert!(store.list(site, DraftKind::Promotion).await.unwrap().is_empty());
        assert_eq!(
            store.list(other, DraftKind::Content).await.unwrap(),
            vec![("draft-1".into(), json!({ "other": true }))]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_in_memory_store_passes_the_conformance_check() {
        conformance::site_draft_store(&InMemorySiteDraftStore::default()).await;
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn the_sqlite_store_passes_the_conformance_check_and_survives_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}?mode=rwc", dir.path().join("drafts.db").display());
        let open = || async {
            let store = SqliteSiteDraftStore::new(sqlx::SqlitePool::connect(&url).await.unwrap());
            store.migrate().await.unwrap();
            store
        };
        conformance::site_draft_store(&open().await).await;
        let site = Uuid::new_v4();
        open()
            .await
            .put(site, DraftKind::Promotion, "promotion-4", &Value::Bool(true))
            .await
            .unwrap();
        assert_eq!(
            open().await.list(site, DraftKind::Promotion).await.unwrap(),
            vec![("promotion-4".to_string(), Value::Bool(true))]
        );
    }

    #[test]
    fn a_stored_draft_reads_back_with_its_media() {
        let draft = Draft {
            change: Change {
                items: Vec::new(),
                media: vec![("content/a/b.png".into(), bytes::Bytes::from_static(&[0, 159, 255]))],
                entry: EntryPoint::WebEditor,
                proposer: "amina".into(),
                job: "content".into(),
                action_class: "publish".into(),
            },
            ids: Default::default(),
        };
        let stored = StoredDraft::of(&draft);
        let back: StoredDraft =
            serde_json::from_value(serde_json::to_value(&stored).unwrap()).unwrap();
        let read = back.into_draft().unwrap();
        assert_eq!(read.change.media, draft.change.media);
        assert_eq!(
            (read.change.proposer, read.change.entry),
            ("amina".into(), EntryPoint::WebEditor)
        );
    }
}
