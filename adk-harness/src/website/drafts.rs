//! The back office's drafts, kept durable (spec 3.5 §7.8.2 step 1, §7.6, §7.11): the store is
//! the source of truth for what a [`SiteRuntime`](super::SiteRuntime) holds between tool
//! calls, so it survives a restart and every replica over one store sees the same drafts.
//!
//! | Kind | What is kept |
//! |---|---|
//! | [`DraftKind::Content`] | A content draft: its items, its media, where it came from, its proposer, job and action class |
//! | [`DraftKind::Charter`] | A charter draft |
//! | [`DraftKind::CharterSign`] | A charter signing request and the signatures it has |
//! | [`DraftKind::Promotion`] | A request that the owner promote a job |
//!
//! A [`SiteDraftStore`] keeps each record as JSON by site, kind and id, with a version:
//!
//! | Operation | Contract |
//! |---|---|
//! | [`create`](SiteDraftStore::create) | Allocates the id `{prefix}-{n}` from a per-site, per-kind counter that never hands a number out twice, at version 1 |
//! | [`replace`](SiteDraftStore::replace) | A compare-and-swap on the version: of two writers at one version exactly one succeeds |
//! | [`list`](SiteDraftStore::list) | The site's records of one kind, in creation order |
//!
//! [`InMemorySiteDraftStore`] serves tests and a single process; [`SqliteSiteDraftStore`]
//! (feature `sqlite`) a served site; `PostgresSiteDraftStore` (feature `postgres`) and
//! `RedisSiteDraftStore` (feature `redis`) every replica of one. Every implementation
//! passes the [`conformance`] suite,
//! which [`site_draft_store_conformance_tests!`](crate::site_draft_store_conformance_tests)
//! runs. A preview is not kept: it names the build it staged, so a process makes it again.

use std::collections::BTreeMap;
use std::fmt;

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

    /// The prefix of the ids a store allocates: `draft`, `charter`, `charter-sign` or
    /// `promotion`, so a content draft is `draft-1`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::website::DraftKind;
    ///
    /// assert_eq!(DraftKind::Content.id_prefix(), "draft");
    /// assert_eq!(DraftKind::CharterSign.id_prefix(), "charter-sign");
    /// ```
    pub fn id_prefix(self) -> &'static str {
        match self {
            Self::Content => "draft",
            Self::Charter => "charter",
            Self::CharterSign => "charter-sign",
            Self::Promotion => "promotion",
        }
    }

    /// The number of `id` when the store allocated it for this kind: `7` for `draft-7`.
    pub(crate) fn number(self, id: &str) -> Option<u64> {
        id.strip_prefix(self.id_prefix())?.strip_prefix('-')?.parse().ok()
    }
}

impl fmt::Display for DraftKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Content => "content draft",
            Self::Charter => "charter draft",
            Self::CharterSign => "charter signing request",
            Self::Promotion => "promotion request",
        })
    }
}

/// One stored record: its store-allocated id, its version, and its value.
///
/// # Example
///
/// ```rust
/// use adk_harness::website::StoredRecord;
///
/// let record = StoredRecord { id: "draft-1".into(), version: 1, value: serde_json::json!({}) };
/// assert_eq!(serde_json::to_value(&record).unwrap()["version"], 1);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredRecord {
    /// The id the store allocated, such as `draft-7`.
    pub id: String,
    /// Starts at 1 and grows by one with every [`replace`](SiteDraftStore::replace).
    pub version: u64,
    /// The record.
    pub value: Value,
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
/// let created = store.create(site, DraftKind::Charter, &serde_json::json!({ "v": 1 })).await?;
/// assert_eq!((created.id.as_str(), created.version), ("charter-1", 1));
/// assert!(store.replace(site, DraftKind::Charter, "charter-1", &serde_json::json!({ "v": 2 }), 1).await?);
/// assert!(!store.replace(site, DraftKind::Charter, "charter-1", &serde_json::json!({ "v": 3 }), 1).await?);
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait SiteDraftStore: Send + Sync {
    /// Keeps `value` as a new record of `kind` for `site`, at version 1, under an id the
    /// store allocates: `{prefix}-{n}` with the kind's [`id_prefix`](DraftKind::id_prefix)
    /// and a number no earlier record of the site and kind had.
    ///
    /// # Errors
    ///
    /// The store's error.
    async fn create(&self, site: Uuid, kind: DraftKind, value: &Value) -> Result<StoredRecord>;

    /// The record `id` of `kind` for `site`; `None` when there is none.
    ///
    /// # Errors
    ///
    /// The store's error.
    async fn get(&self, site: Uuid, kind: DraftKind, id: &str) -> Result<Option<StoredRecord>>;

    /// Replaces the record `id` with `value` when it is at `expected_version`, raising its
    /// version by one. `false`, writing nothing, when it is at another version or gone.
    ///
    /// # Errors
    ///
    /// The store's error.
    async fn replace(
        &self,
        site: Uuid,
        kind: DraftKind,
        id: &str,
        value: &Value,
        expected_version: u64,
    ) -> Result<bool>;

    /// Forgets `id` of `kind` for `site`; nothing when there is none. Its number is never
    /// allocated again.
    ///
    /// # Errors
    ///
    /// The store's error.
    async fn remove(&self, site: Uuid, kind: DraftKind, id: &str) -> Result<()>;

    /// Every record of `kind` for `site`, in creation order.
    ///
    /// # Errors
    ///
    /// The store's error.
    async fn list(&self, site: Uuid, kind: DraftKind) -> Result<Vec<StoredRecord>>;
}

#[derive(Debug, Default)]
struct Records {
    /// The last number allocated per site and kind.
    last: BTreeMap<(Uuid, DraftKind), u64>,
    /// By site, kind and number: the id, the version and the value.
    records: BTreeMap<(Uuid, DraftKind, u64), StoredRecord>,
}

/// A [`SiteDraftStore`] in memory, for tests and a single process.
#[derive(Debug, Default)]
pub struct InMemorySiteDraftStore {
    inner: Mutex<Records>,
}

impl InMemorySiteDraftStore {
    fn find(records: &Records, site: Uuid, kind: DraftKind, id: &str) -> Option<u64> {
        kind.number(id).filter(|n| records.records.contains_key(&(site, kind, *n)))
    }
}

#[async_trait]
impl SiteDraftStore for InMemorySiteDraftStore {
    async fn create(&self, site: Uuid, kind: DraftKind, value: &Value) -> Result<StoredRecord> {
        let mut inner = self.inner.lock().await;
        let last = inner.last.entry((site, kind)).or_default();
        *last += 1;
        let n = *last;
        let record = StoredRecord {
            id: format!("{}-{n}", kind.id_prefix()),
            version: 1,
            value: value.clone(),
        };
        inner.records.insert((site, kind, n), record.clone());
        Ok(record)
    }

    async fn get(&self, site: Uuid, kind: DraftKind, id: &str) -> Result<Option<StoredRecord>> {
        let inner = self.inner.lock().await;
        Ok(Self::find(&inner, site, kind, id)
            .and_then(|n| inner.records.get(&(site, kind, n)).cloned()))
    }

    async fn replace(
        &self,
        site: Uuid,
        kind: DraftKind,
        id: &str,
        value: &Value,
        expected_version: u64,
    ) -> Result<bool> {
        let mut inner = self.inner.lock().await;
        let Some(n) = Self::find(&inner, site, kind, id) else { return Ok(false) };
        match inner.records.get_mut(&(site, kind, n)) {
            Some(record) if record.version == expected_version => {
                record.value = value.clone();
                record.version += 1;
                Ok(true)
            }
            Some(_) | None => Ok(false),
        }
    }

    async fn remove(&self, site: Uuid, kind: DraftKind, id: &str) -> Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(n) = Self::find(&inner, site, kind, id) {
            inner.records.remove(&(site, kind, n));
        }
        Ok(())
    }

    async fn list(&self, site: Uuid, kind: DraftKind) -> Result<Vec<StoredRecord>> {
        Ok(self
            .inner
            .lock()
            .await
            .records
            .range((site, kind, 0)..=(site, kind, u64::MAX))
            .map(|(_, record)| record.clone())
            .collect())
    }
}

pub(crate) fn store_error(error: impl fmt::Display) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::Unavailable,
        "website.drafts.store",
        format!("the site's draft store: {error}"),
    )
}

/// The error of a write that lost its compare-and-swap: the record changed since it was read.
pub(crate) fn conflict(kind: DraftKind, id: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::Conflict,
        "website.drafts.conflict",
        format!(
            "the {kind} '{id}' changed or was removed while this change was made; read it \
             again and repeat the change"
        ),
    )
}

/// A stored version as SQL's signed integer.
#[cfg(any(feature = "sqlite", feature = "postgres"))]
fn sql_version(version: u64) -> Result<i64> {
    i64::try_from(version).map_err(|_| store_error(format!("version {version} is out of range")))
}

/// A [`SiteDraftStore`] in SQLite, such as a served site's `data/edge.db` (feature
/// `sqlite`).
///
/// [`migrate`](Self::migrate) upgrades a table an earlier release created in place: its
/// records start at version 1, and new ids continue after the highest one it holds.
///
/// # Example
///
/// ```rust,ignore
/// let drafts = SqliteSiteDraftStore::new(pool);
/// drafts.migrate().await?;
/// let runtime = SiteRuntime::builder(site, base, store, generator, host, log, work)
///     .drafts(Arc::new(drafts))
///     .build();
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

    /// Creates the tables, or upgrades the table of an earlier release.
    ///
    /// # Errors
    ///
    /// The database's error.
    pub async fn migrate(&self) -> Result<()> {
        for statement in [
            "CREATE TABLE IF NOT EXISTS adk_website_drafts (
                site TEXT NOT NULL,
                kind TEXT NOT NULL,
                id TEXT NOT NULL,
                value TEXT NOT NULL,
                version INTEGER NOT NULL DEFAULT 1,
                seq INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (site, kind, id)
            )",
            "CREATE TABLE IF NOT EXISTS adk_website_draft_ids (
                site TEXT NOT NULL,
                kind TEXT NOT NULL,
                last INTEGER NOT NULL,
                PRIMARY KEY (site, kind)
            )",
        ] {
            sqlx::query(statement).execute(&self.pool).await.map_err(store_error)?;
        }
        // A table from before versions gains `version` and `seq`; its ids carry the numbers.
        for (column, statement) in [
            (
                "version",
                "ALTER TABLE adk_website_drafts ADD COLUMN version INTEGER NOT NULL DEFAULT 1",
            ),
            ("seq", "ALTER TABLE adk_website_drafts ADD COLUMN seq INTEGER NOT NULL DEFAULT 0"),
        ] {
            if !self.has_column(column).await? {
                let added = sqlx::query(statement).execute(&self.pool).await;
                // Another process may have added it first.
                if let Err(error) = added
                    && !self.has_column(column).await?
                {
                    return Err(store_error(error));
                }
            }
        }
        let unnumbered: Vec<(String, String, String)> =
            sqlx::query_as("SELECT site, kind, id FROM adk_website_drafts WHERE seq = 0")
                .fetch_all(&self.pool)
                .await
                .map_err(store_error)?;
        for (site, kind, id) in unnumbered {
            let Some(n) = DraftKind::ALL
                .into_iter()
                .find(|known| known.as_str() == kind)
                .and_then(|known| known.number(&id))
            else {
                continue;
            };
            sqlx::query(
                "UPDATE adk_website_drafts SET seq = ? WHERE site = ? AND kind = ? AND id = ?",
            )
            .bind(sql_version(n)?)
            .bind(&site)
            .bind(&kind)
            .bind(&id)
            .execute(&self.pool)
            .await
            .map_err(store_error)?;
        }
        // New ids continue after the highest number already stored.
        sqlx::query(
            "INSERT INTO adk_website_draft_ids (site, kind, last)
             SELECT site, kind, MAX(seq) FROM adk_website_drafts WHERE true GROUP BY site, kind
             ON CONFLICT (site, kind) DO UPDATE
             SET last = MAX(adk_website_draft_ids.last, excluded.last)",
        )
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn has_column(&self, column: &str) -> Result<bool> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pragma_table_info('adk_website_drafts') WHERE name = ?",
        )
        .bind(column)
        .fetch_one(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(count > 0)
    }
}

#[cfg(any(feature = "sqlite", feature = "postgres"))]
fn stored(id: String, version: i64, value: &str) -> Result<StoredRecord> {
    Ok(StoredRecord {
        id,
        version: u64::try_from(version).map_err(store_error)?,
        value: serde_json::from_str(value).map_err(store_error)?,
    })
}

#[cfg(feature = "sqlite")]
#[async_trait]
impl SiteDraftStore for SqliteSiteDraftStore {
    async fn create(&self, site: Uuid, kind: DraftKind, value: &Value) -> Result<StoredRecord> {
        let (n,): (i64,) = sqlx::query_as(
            "INSERT INTO adk_website_draft_ids (site, kind, last) VALUES (?, ?, 1)
             ON CONFLICT (site, kind) DO UPDATE SET last = adk_website_draft_ids.last + 1
             RETURNING last",
        )
        .bind(site.to_string())
        .bind(kind.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(store_error)?;
        let id = format!("{}-{n}", kind.id_prefix());
        sqlx::query(
            "INSERT INTO adk_website_drafts (site, kind, id, value, version, seq)
             VALUES (?, ?, ?, ?, 1, ?)",
        )
        .bind(site.to_string())
        .bind(kind.as_str())
        .bind(&id)
        .bind(value.to_string())
        .bind(n)
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(StoredRecord { id, version: 1, value: value.clone() })
    }

    async fn get(&self, site: Uuid, kind: DraftKind, id: &str) -> Result<Option<StoredRecord>> {
        let row: Option<(i64, String)> = sqlx::query_as(
            "SELECT version, value FROM adk_website_drafts WHERE site = ? AND kind = ? AND id = ?",
        )
        .bind(site.to_string())
        .bind(kind.as_str())
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_error)?;
        row.map(|(version, value)| stored(id.to_string(), version, &value)).transpose()
    }

    async fn replace(
        &self,
        site: Uuid,
        kind: DraftKind,
        id: &str,
        value: &Value,
        expected_version: u64,
    ) -> Result<bool> {
        let updated = sqlx::query(
            "UPDATE adk_website_drafts SET value = ?, version = version + 1
             WHERE site = ? AND kind = ? AND id = ? AND version = ?",
        )
        .bind(value.to_string())
        .bind(site.to_string())
        .bind(kind.as_str())
        .bind(id)
        .bind(sql_version(expected_version)?)
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(updated.rows_affected() == 1)
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

    async fn list(&self, site: Uuid, kind: DraftKind) -> Result<Vec<StoredRecord>> {
        let rows: Vec<(String, i64, String)> = sqlx::query_as(
            "SELECT id, version, value FROM adk_website_drafts
             WHERE site = ? AND kind = ? ORDER BY seq, id",
        )
        .bind(site.to_string())
        .bind(kind.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        rows.into_iter().map(|(id, version, value)| stored(id, version, &value)).collect()
    }
}

#[cfg(feature = "postgres")]
const POSTGRES_DRAFT_MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create site draft tables",
    "CREATE TABLE IF NOT EXISTS adk_website_drafts (\
        site TEXT NOT NULL, kind TEXT NOT NULL, id TEXT NOT NULL, value TEXT NOT NULL, \
        version BIGINT NOT NULL, seq BIGINT NOT NULL, \
        PRIMARY KEY (site, kind, id)); \
     CREATE INDEX IF NOT EXISTS adk_website_drafts_in_order \
        ON adk_website_drafts (site, kind, seq); \
     CREATE TABLE IF NOT EXISTS adk_website_draft_ids (\
        site TEXT NOT NULL, kind TEXT NOT NULL, last BIGINT NOT NULL, \
        PRIMARY KEY (site, kind));",
)];

/// A [`SiteDraftStore`] in PostgreSQL, shared by every replica of a served site (feature
/// `postgres`).
///
/// Records live in `adk_website_drafts` by site, kind and id with their version; the
/// counter that allocates ids is a row per site and kind in `adk_website_draft_ids`,
/// incremented by an upsert, so replicas creating at once get distinct ids.
/// [`migrate`](Self::migrate) records its steps in `_adk_website_draft_migrations` under an
/// advisory lock, so replicas starting together migrate one at a time.
///
/// # Example
///
/// ```rust,no_run
/// use adk_harness::website::{DraftKind, PostgresSiteDraftStore, SiteDraftStore};
///
/// # async fn example() -> adk_core::Result<()> {
/// let store = PostgresSiteDraftStore::connect("postgres://localhost/site").await?;
/// store.migrate().await?;
/// let drafts = store.list(uuid::Uuid::nil(), DraftKind::Content).await?;
/// # let _ = drafts;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "postgres")]
#[derive(Debug, Clone)]
pub struct PostgresSiteDraftStore {
    pool: sqlx::PgPool,
}

#[cfg(feature = "postgres")]
impl PostgresSiteDraftStore {
    /// A store over `pool`, such as the one the site's other stores use; call
    /// [`migrate`](Self::migrate) once.
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    /// Connects to `database_url` with a pool of its own.
    ///
    /// # Errors
    ///
    /// `website.drafts.store` when the server cannot be reached.
    pub async fn connect(database_url: &str) -> Result<Self> {
        Ok(Self::new(sqlx::PgPool::connect(database_url).await.map_err(store_error)?))
    }

    /// Creates or upgrades the store's tables.
    ///
    /// # Errors
    ///
    /// The database's error, or a version mismatch when a newer release migrated the
    /// database.
    pub async fn migrate(&self) -> Result<()> {
        crate::fsutil::migrate_postgres(
            &self.pool,
            "_adk_website_draft_migrations",
            POSTGRES_DRAFT_MIGRATIONS,
        )
        .await
    }
}

#[cfg(feature = "postgres")]
#[async_trait]
impl SiteDraftStore for PostgresSiteDraftStore {
    async fn create(&self, site: Uuid, kind: DraftKind, value: &Value) -> Result<StoredRecord> {
        let mut tx = self.pool.begin().await.map_err(store_error)?;
        let (n,): (i64,) = sqlx::query_as(
            "INSERT INTO adk_website_draft_ids (site, kind, last) VALUES ($1, $2, 1)
             ON CONFLICT (site, kind) DO UPDATE SET last = adk_website_draft_ids.last + 1
             RETURNING last",
        )
        .bind(site.to_string())
        .bind(kind.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(store_error)?;
        let id = format!("{}-{n}", kind.id_prefix());
        sqlx::query(
            "INSERT INTO adk_website_drafts (site, kind, id, value, version, seq)
             VALUES ($1, $2, $3, $4, 1, $5)",
        )
        .bind(site.to_string())
        .bind(kind.as_str())
        .bind(&id)
        .bind(value.to_string())
        .bind(n)
        .execute(&mut *tx)
        .await
        .map_err(store_error)?;
        tx.commit().await.map_err(store_error)?;
        Ok(StoredRecord { id, version: 1, value: value.clone() })
    }

    async fn get(&self, site: Uuid, kind: DraftKind, id: &str) -> Result<Option<StoredRecord>> {
        let row: Option<(i64, String)> = sqlx::query_as(
            "SELECT version, value FROM adk_website_drafts
             WHERE site = $1 AND kind = $2 AND id = $3",
        )
        .bind(site.to_string())
        .bind(kind.as_str())
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_error)?;
        row.map(|(version, value)| stored(id.to_string(), version, &value)).transpose()
    }

    async fn replace(
        &self,
        site: Uuid,
        kind: DraftKind,
        id: &str,
        value: &Value,
        expected_version: u64,
    ) -> Result<bool> {
        let updated = sqlx::query(
            "UPDATE adk_website_drafts SET value = $1, version = version + 1
             WHERE site = $2 AND kind = $3 AND id = $4 AND version = $5",
        )
        .bind(value.to_string())
        .bind(site.to_string())
        .bind(kind.as_str())
        .bind(id)
        .bind(sql_version(expected_version)?)
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(updated.rows_affected() == 1)
    }

    async fn remove(&self, site: Uuid, kind: DraftKind, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM adk_website_drafts WHERE site = $1 AND kind = $2 AND id = $3")
            .bind(site.to_string())
            .bind(kind.as_str())
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(store_error)?;
        Ok(())
    }

    async fn list(&self, site: Uuid, kind: DraftKind) -> Result<Vec<StoredRecord>> {
        let rows: Vec<(String, i64, String)> = sqlx::query_as(
            "SELECT id, version, value FROM adk_website_drafts
             WHERE site = $1 AND kind = $2 ORDER BY seq, id",
        )
        .bind(site.to_string())
        .bind(kind.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        rows.into_iter().map(|(id, version, value)| stored(id, version, &value)).collect()
    }
}

/// KEYS: counters, values, versions, order. ARGV: kind, id prefix, value JSON. Allocates the
/// kind's next number and stores the record at version 1; returns its id.
#[cfg(feature = "redis")]
const CREATE_DRAFT_SCRIPT: &str = r#"
local n = redis.call('HINCRBY', KEYS[1], ARGV[1], 1)
local id = ARGV[2] .. '-' .. n
redis.call('HSET', KEYS[2], id, ARGV[3])
redis.call('HSET', KEYS[3], id, 1)
redis.call('ZADD', KEYS[4], n, id)
return id
"#;

/// KEYS: values, versions. ARGV: id. Returns the record's version and value, or nothing.
#[cfg(feature = "redis")]
const GET_DRAFT_SCRIPT: &str = r#"
local value = redis.call('HGET', KEYS[1], ARGV[1])
if not value then return {} end
return { redis.call('HGET', KEYS[2], ARGV[1]), value }
"#;

/// KEYS: values, versions. ARGV: id, value JSON, expected version. Writes the value and
/// raises the version when the record is at the expected one; returns 1 when it wrote.
#[cfg(feature = "redis")]
const REPLACE_DRAFT_SCRIPT: &str = r#"
local version = redis.call('HGET', KEYS[2], ARGV[1])
if not version or version ~= ARGV[3] then return 0 end
redis.call('HSET', KEYS[1], ARGV[1], ARGV[2])
redis.call('HINCRBY', KEYS[2], ARGV[1], 1)
return 1
"#;

/// KEYS: values, versions, order. ARGV: id. Forgets the record.
#[cfg(feature = "redis")]
const REMOVE_DRAFT_SCRIPT: &str = r#"
redis.call('HDEL', KEYS[1], ARGV[1])
redis.call('HDEL', KEYS[2], ARGV[1])
redis.call('ZREM', KEYS[3], ARGV[1])
return 1
"#;

/// KEYS: values, versions, order. Returns id, version and value of every record in creation
/// order, read atomically.
#[cfg(feature = "redis")]
const LIST_DRAFTS_SCRIPT: &str = r#"
local out = {}
for _, id in ipairs(redis.call('ZRANGE', KEYS[3], 0, -1)) do
  local value = redis.call('HGET', KEYS[1], id)
  if value then
    table.insert(out, id)
    table.insert(out, redis.call('HGET', KEYS[2], id))
    table.insert(out, value)
  end
end
return out
"#;

/// A [`SiteDraftStore`] in Redis, shared by every replica of a served site (feature
/// `redis`).
///
/// A site's keys share its hash tag, so they sit in one Redis Cluster slot and every change
/// runs as one atomic script:
///
/// | Key | Type | Contents |
/// |-----|------|----------|
/// | `<namespace>:{<site>}:draft-ids` | Hash | Kind → the last number allocated |
/// | `<namespace>:{<site>}:drafts:<kind>` | Hash | Id → value JSON |
/// | `<namespace>:{<site>}:draft-versions:<kind>` | Hash | Id → version |
/// | `<namespace>:{<site>}:draft-order:<kind>` | Sorted set | Ids by number, for creation order |
///
/// The namespace is `adk:harness` unless [`with_namespace`](Self::with_namespace) sets
/// another.
///
/// # Example
///
/// ```rust,no_run
/// use adk_harness::website::{DraftKind, RedisSiteDraftStore, SiteDraftStore};
///
/// # async fn example() -> adk_core::Result<()> {
/// let store = RedisSiteDraftStore::connect("redis://localhost:6379").await?;
/// store.migrate().await?;
/// let drafts = store.list(uuid::Uuid::nil(), DraftKind::Content).await?;
/// # let _ = drafts;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "redis")]
#[derive(Debug, Clone)]
pub struct RedisSiteDraftStore {
    client: fred::prelude::Client,
    namespace: String,
}

#[cfg(feature = "redis")]
fn redis_error(action: &str, error: &dyn fmt::Display) -> AdkError {
    store_error(format!("could not {action}: {error}"))
}

#[cfg(feature = "redis")]
impl RedisSiteDraftStore {
    /// A store over an initialized client, such as one shared with the harness's other
    /// Redis stores.
    pub fn from_client(client: fred::prelude::Client) -> Self {
        Self { client, namespace: crate::fsutil::REDIS_NAMESPACE.to_string() }
    }

    /// Connects to `url`.
    ///
    /// # Errors
    ///
    /// `harness.store.database` when the server cannot be reached.
    pub async fn connect(url: &str) -> Result<Self> {
        Ok(Self::from_client(crate::fsutil::redis_connect(url).await?))
    }

    /// Keeps the store's keys under `namespace` instead of `adk:harness`, for example to
    /// separate deployments that share a server.
    #[must_use]
    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = crate::fsutil::redis_namespace(&namespace.into());
        self
    }

    /// Loads the store's scripts into the server's script cache.
    ///
    /// # Errors
    ///
    /// `website.drafts.store` when the server rejects a script.
    pub async fn migrate(&self) -> Result<()> {
        use fred::prelude::LuaInterface as _;
        for script in [
            CREATE_DRAFT_SCRIPT,
            GET_DRAFT_SCRIPT,
            REPLACE_DRAFT_SCRIPT,
            REMOVE_DRAFT_SCRIPT,
            LIST_DRAFTS_SCRIPT,
        ] {
            let _: String = self
                .client
                .script_load(script)
                .await
                .map_err(|error| redis_error("load a script", &error))?;
        }
        Ok(())
    }

    /// The site's counters key, then the kind's values, versions and order keys.
    fn keys(&self, site: Uuid, kind: DraftKind) -> [String; 4] {
        let prefix =
            format!("{}:{{{}}}", self.namespace, crate::fsutil::redis_segment(&site.to_string()));
        let kind = kind.as_str();
        [
            format!("{prefix}:draft-ids"),
            format!("{prefix}:drafts:{kind}"),
            format!("{prefix}:draft-versions:{kind}"),
            format!("{prefix}:draft-order:{kind}"),
        ]
    }
}

#[cfg(feature = "redis")]
fn redis_record(id: String, version: &str, value: &str) -> Result<StoredRecord> {
    Ok(StoredRecord {
        id,
        version: version.parse().map_err(store_error)?,
        value: serde_json::from_str(value).map_err(store_error)?,
    })
}

#[cfg(feature = "redis")]
#[async_trait]
impl SiteDraftStore for RedisSiteDraftStore {
    async fn create(&self, site: Uuid, kind: DraftKind, value: &Value) -> Result<StoredRecord> {
        use fred::prelude::LuaInterface as _;
        let id: String = self
            .client
            .eval(
                CREATE_DRAFT_SCRIPT,
                self.keys(site, kind).to_vec(),
                vec![kind.as_str().to_string(), kind.id_prefix().to_string(), value.to_string()],
            )
            .await
            .map_err(|error| redis_error("create a draft", &error))?;
        Ok(StoredRecord { id, version: 1, value: value.clone() })
    }

    async fn get(&self, site: Uuid, kind: DraftKind, id: &str) -> Result<Option<StoredRecord>> {
        use fred::prelude::LuaInterface as _;
        let [_, values, versions, _] = self.keys(site, kind);
        let reply: Vec<String> = self
            .client
            .eval(GET_DRAFT_SCRIPT, vec![values, versions], vec![id.to_string()])
            .await
            .map_err(|error| redis_error("read a draft", &error))?;
        match reply.as_slice() {
            [version, value] => Ok(Some(redis_record(id.to_string(), version, value)?)),
            _ => Ok(None),
        }
    }

    async fn replace(
        &self,
        site: Uuid,
        kind: DraftKind,
        id: &str,
        value: &Value,
        expected_version: u64,
    ) -> Result<bool> {
        use fred::prelude::LuaInterface as _;
        let [_, values, versions, _] = self.keys(site, kind);
        let written: i64 = self
            .client
            .eval(
                REPLACE_DRAFT_SCRIPT,
                vec![values, versions],
                vec![id.to_string(), value.to_string(), expected_version.to_string()],
            )
            .await
            .map_err(|error| redis_error("replace a draft", &error))?;
        Ok(written == 1)
    }

    async fn remove(&self, site: Uuid, kind: DraftKind, id: &str) -> Result<()> {
        use fred::prelude::LuaInterface as _;
        let [_, values, versions, order] = self.keys(site, kind);
        let _: i64 = self
            .client
            .eval(REMOVE_DRAFT_SCRIPT, vec![values, versions, order], vec![id.to_string()])
            .await
            .map_err(|error| redis_error("remove a draft", &error))?;
        Ok(())
    }

    async fn list(&self, site: Uuid, kind: DraftKind) -> Result<Vec<StoredRecord>> {
        use fred::prelude::LuaInterface as _;
        let [_, values, versions, order] = self.keys(site, kind);
        let reply: Vec<String> = self
            .client
            .eval(LIST_DRAFTS_SCRIPT, vec![values, versions, order], Vec::<String>::new())
            .await
            .map_err(|error| redis_error("list drafts", &error))?;
        reply
            .chunks(3)
            .map(|record| match record {
                [id, version, value] => redis_record(id.clone(), version, value),
                _ => Err(store_error("a draft listing came back incomplete")),
            })
            .collect()
    }
}

/// A content draft as it is stored: the change, its media base64-encoded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoredDraft {
    pub(crate) items: Vec<ContentItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    media: Vec<(String, String)>,
    pub(crate) entry: EntryPoint,
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

/// The conformance suite every [`SiteDraftStore`] passes (spec 3.5 §7.8.2), run with
/// [`site_draft_store_conformance_tests!`](crate::site_draft_store_conformance_tests).
///
/// | Case | Contract |
/// |------|----------|
/// | [`records_are_kept_apart_in_creation_order`] | Records are kept by site and kind, listed in creation order, read back by id, and removed idempotently |
/// | [`ids_are_never_reused`] | A removed record's number, the highest one included, is never allocated again |
/// | [`concurrent_creates_are_distinct`] | Records created at once across two handles get distinct ids |
/// | [`replace_is_a_compare_and_swap`] | `replace` writes only at the expected version, raising it by one, and never revives a removed record |
/// | [`concurrent_replace_has_one_winner`] | Of two replaces at one version across two handles, exactly one succeeds |
/// | [`records_survive_a_restart`] | Records, versions and the id counter read back unchanged through a new handle |
///
/// Every case uses sites of its own, so cases share one database.
pub mod conformance {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::json;
    use uuid::Uuid;

    use super::{DraftKind, SiteDraftStore, StoredRecord};

    /// Opens a [`SiteDraftStore`] backend for the conformance suite.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use adk_harness::website::draft_conformance::SiteDraftStoreHarness;
    /// use adk_harness::website::{InMemorySiteDraftStore, SiteDraftStore};
    ///
    /// struct InMemory(Arc<InMemorySiteDraftStore>);
    ///
    /// #[async_trait::async_trait]
    /// impl SiteDraftStoreHarness for InMemory {
    ///     async fn open(&self) -> Arc<dyn SiteDraftStore> {
    ///         self.0.clone()
    ///     }
    /// }
    /// ```
    #[async_trait]
    pub trait SiteDraftStoreHarness: Send + Sync {
        /// Opens the store.
        ///
        /// For a durable backend every call returns a new handle over the same stored data,
        /// as a restarted process or a second replica sees it. A process-local backend
        /// returns the same store every time. Panics when the backend cannot be opened.
        async fn open(&self) -> Arc<dyn SiteDraftStore>;
    }

    fn ids(records: &[StoredRecord]) -> Vec<&str> {
        records.iter().map(|record| record.id.as_str()).collect()
    }

    /// Records are kept apart by site and kind, listed in creation order — `draft-10` after
    /// `draft-9` — read back by id, and removed idempotently.
    ///
    /// # Panics
    ///
    /// When the store breaks its contract.
    pub async fn records_are_kept_apart_in_creation_order<H: SiteDraftStoreHarness + ?Sized>(
        harness: &H,
    ) {
        let store = harness.open().await;
        let (site, other) = (Uuid::new_v4(), Uuid::new_v4());
        let mut created = Vec::new();
        for n in 1..=10 {
            created.push(store.create(site, DraftKind::Content, &json!({ "n": n })).await.unwrap());
        }
        assert_eq!(created[0].id, "draft-1");
        assert_eq!(created[9].id, "draft-10");
        assert!(created.iter().all(|record| record.version == 1), "created at version 1");
        let charter = store.create(site, DraftKind::Charter, &json!({ "c": 1 })).await.unwrap();
        assert_eq!(charter.id, "charter-1", "each kind counts on its own");
        let elsewhere =
            store.create(other, DraftKind::Content, &json!({ "other": 1 })).await.unwrap();
        assert_eq!(elsewhere.id, "draft-1", "each site counts on its own");

        let listed = store.list(site, DraftKind::Content).await.unwrap();
        assert_eq!(listed, created, "listed in creation order, this site's and kind's only");
        assert_eq!(store.list(site, DraftKind::Charter).await.unwrap(), vec![charter.clone()]);
        assert!(store.list(site, DraftKind::Promotion).await.unwrap().is_empty());
        assert_eq!(store.list(other, DraftKind::Content).await.unwrap(), vec![elsewhere]);
        assert_eq!(
            store.get(site, DraftKind::Content, "draft-2").await.unwrap(),
            Some(created[1].clone())
        );
        assert_eq!(store.get(site, DraftKind::CharterSign, "charter-1").await.unwrap(), None);
        assert_eq!(store.get(other, DraftKind::Charter, "charter-1").await.unwrap(), None);

        store.remove(site, DraftKind::Content, "draft-2").await.unwrap();
        store.remove(site, DraftKind::Content, "draft-2").await.unwrap();
        store.remove(site, DraftKind::Content, "draft-99").await.unwrap();
        assert_eq!(store.get(site, DraftKind::Content, "draft-2").await.unwrap(), None);
        let left = store.list(site, DraftKind::Content).await.unwrap();
        assert_eq!(left.len(), 9);
        assert!(!ids(&left).contains(&"draft-2"));
        assert_eq!(store.get(site, DraftKind::Charter, "charter-1").await.unwrap(), Some(charter));
    }

    /// A removed record's number is never allocated again, the highest one included.
    ///
    /// # Panics
    ///
    /// When the store breaks its contract.
    pub async fn ids_are_never_reused<H: SiteDraftStoreHarness + ?Sized>(harness: &H) {
        let store = harness.open().await;
        let site = Uuid::new_v4();
        for n in 1..=3 {
            store.create(site, DraftKind::Promotion, &json!(n)).await.unwrap();
        }
        for id in ["promotion-3", "promotion-2", "promotion-1"] {
            store.remove(site, DraftKind::Promotion, id).await.unwrap();
        }
        let next = store.create(site, DraftKind::Promotion, &json!(4)).await.unwrap();
        assert_eq!(next.id, "promotion-4", "numbers of removed records are not handed out again");
        assert_eq!(ids(&store.list(site, DraftKind::Promotion).await.unwrap()), ["promotion-4"]);
    }

    /// Records created at once across two handles get distinct ids, and every one is kept.
    ///
    /// # Panics
    ///
    /// When the store breaks its contract.
    pub async fn concurrent_creates_are_distinct<H: SiteDraftStoreHarness + ?Sized>(harness: &H) {
        let (first, second) = (harness.open().await, harness.open().await);
        let site = Uuid::new_v4();
        let creates = (0..20).map(|n| {
            let store = if n % 2 == 0 { first.clone() } else { second.clone() };
            async move { store.create(site, DraftKind::Content, &json!({ "n": n })).await }
        });
        let created: Vec<StoredRecord> = futures::future::join_all(creates)
            .await
            .into_iter()
            .collect::<adk_core::Result<_>>()
            .unwrap();
        let distinct: BTreeSet<&str> = ids(&created).into_iter().collect();
        assert_eq!(distinct.len(), 20, "every create got its own id: {distinct:?}");
        assert_eq!(first.list(site, DraftKind::Content).await.unwrap().len(), 20);
    }

    /// `replace` writes only at the expected version and raises it by one; a stale version or
    /// a removed record writes nothing.
    ///
    /// # Panics
    ///
    /// When the store breaks its contract.
    pub async fn replace_is_a_compare_and_swap<H: SiteDraftStoreHarness + ?Sized>(harness: &H) {
        let store = harness.open().await;
        let site = Uuid::new_v4();
        let created = store.create(site, DraftKind::Charter, &json!({ "v": 1 })).await.unwrap();
        assert!(
            store
                .replace(site, DraftKind::Charter, &created.id, &json!({ "v": 2 }), 1)
                .await
                .unwrap()
        );
        assert!(
            !store
                .replace(site, DraftKind::Charter, &created.id, &json!({ "v": 3 }), 1)
                .await
                .unwrap(),
            "a stale version writes nothing"
        );
        assert!(
            !store
                .replace(site, DraftKind::Content, &created.id, &json!({ "v": 3 }), 2)
                .await
                .unwrap(),
            "another kind's record is not this one"
        );
        assert_eq!(
            store.get(site, DraftKind::Charter, &created.id).await.unwrap(),
            Some(StoredRecord { id: created.id.clone(), version: 2, value: json!({ "v": 2 }) })
        );
        store.remove(site, DraftKind::Charter, &created.id).await.unwrap();
        assert!(
            !store
                .replace(site, DraftKind::Charter, &created.id, &json!({ "v": 4 }), 2)
                .await
                .unwrap(),
            "a removed record is not revived"
        );
        assert_eq!(store.get(site, DraftKind::Charter, &created.id).await.unwrap(), None);
    }

    /// Of two replaces at one version, across two handles, exactly one succeeds.
    ///
    /// # Panics
    ///
    /// When the store breaks its contract.
    pub async fn concurrent_replace_has_one_winner<H: SiteDraftStoreHarness + ?Sized>(harness: &H) {
        let (first, second) = (harness.open().await, harness.open().await);
        let site = Uuid::new_v4();
        for _ in 0..5 {
            let created = first.create(site, DraftKind::Content, &json!("base")).await.unwrap();
            let (one, two) = (json!("first"), json!("second"));
            let (a, b) = futures::join!(
                first.replace(site, DraftKind::Content, &created.id, &one, 1),
                second.replace(site, DraftKind::Content, &created.id, &two, 1),
            );
            let (a, b) = (a.unwrap(), b.unwrap());
            assert!(a ^ b, "exactly one replace wins: {a} {b}");
            let stored = second.get(site, DraftKind::Content, &created.id).await.unwrap().unwrap();
            assert_eq!(stored.version, 2);
            assert_eq!(stored.value, if a { one } else { two });
        }
    }

    /// Records, their versions and the id counter read back unchanged through a new handle.
    ///
    /// # Panics
    ///
    /// When the store breaks its contract.
    pub async fn records_survive_a_restart<H: SiteDraftStoreHarness + ?Sized>(harness: &H) {
        let site = Uuid::new_v4();
        let (kept, edited) = {
            let store = harness.open().await;
            let kept =
                store.create(site, DraftKind::CharterSign, &json!({ "k": 1 })).await.unwrap();
            let edited =
                store.create(site, DraftKind::CharterSign, &json!({ "k": 2 })).await.unwrap();
            assert!(
                store
                    .replace(site, DraftKind::CharterSign, &edited.id, &json!({ "k": 3 }), 1)
                    .await
                    .unwrap()
            );
            (kept, edited)
        };
        let reopened = harness.open().await;
        assert_eq!(
            reopened.list(site, DraftKind::CharterSign).await.unwrap(),
            vec![kept, StoredRecord { id: edited.id, version: 2, value: json!({ "k": 3 }) }]
        );
        let next = reopened.create(site, DraftKind::CharterSign, &json!({ "k": 4 })).await.unwrap();
        assert_eq!(next.id, "charter-sign-3", "the counter survives the restart");
    }
}

/// Generates one test per [`conformance`](crate::website::draft_conformance) case for a
/// [`SiteDraftStoreHarness`](crate::website::draft_conformance::SiteDraftStoreHarness).
///
/// # Example
///
/// ```rust,ignore
/// adk_harness::site_draft_store_conformance_tests!(harness = SqliteHarness::new());
/// ```
#[macro_export]
macro_rules! site_draft_store_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn site_drafts_are_kept_apart_in_creation_order() {
            $crate::website::draft_conformance::records_are_kept_apart_in_creation_order(
                &$harness,
            )
            .await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn site_draft_ids_are_never_reused() {
            $crate::website::draft_conformance::ids_are_never_reused(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn site_draft_concurrent_creates_are_distinct() {
            $crate::website::draft_conformance::concurrent_creates_are_distinct(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn site_draft_replace_is_a_compare_and_swap() {
            $crate::website::draft_conformance::replace_is_a_compare_and_swap(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn site_draft_concurrent_replace_has_one_winner() {
            $crate::website::draft_conformance::concurrent_replace_has_one_winner(&$harness)
                .await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn site_drafts_survive_a_restart() {
            $crate::website::draft_conformance::records_survive_a_restart(&$harness).await;
        }
    };
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::conformance::SiteDraftStoreHarness;
    use super::*;

    struct InMemory(Arc<InMemorySiteDraftStore>);

    #[async_trait]
    impl SiteDraftStoreHarness for InMemory {
        async fn open(&self) -> Arc<dyn SiteDraftStore> {
            self.0.clone()
        }
    }

    mod in_memory {
        use super::*;

        crate::site_draft_store_conformance_tests!(
            harness = InMemory(Arc::new(InMemorySiteDraftStore::default()))
        );
    }

    #[cfg(feature = "sqlite")]
    mod sqlite {
        use super::*;

        /// A database file every `open` connects to anew, as a second process does.
        struct Sqlite(tempfile::TempDir);

        impl Sqlite {
            fn new() -> Self {
                Self(tempfile::tempdir().unwrap())
            }

            fn url(&self) -> String {
                format!("sqlite://{}?mode=rwc", self.0.path().join("drafts.db").display())
            }
        }

        #[async_trait]
        impl SiteDraftStoreHarness for Sqlite {
            async fn open(&self) -> Arc<dyn SiteDraftStore> {
                let pool = sqlx::SqlitePool::connect(&self.url()).await.unwrap();
                let store = SqliteSiteDraftStore::new(pool);
                store.migrate().await.unwrap();
                Arc::new(store)
            }
        }

        crate::site_draft_store_conformance_tests!(harness = Sqlite::new());

        #[tokio::test]
        async fn a_table_from_before_versions_is_upgraded_in_place() {
            let harness = Sqlite::new();
            let pool = sqlx::SqlitePool::connect(&harness.url()).await.unwrap();
            // The table as the first release created it.
            sqlx::query(
                "CREATE TABLE adk_website_drafts (
                    site TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    id TEXT NOT NULL,
                    value TEXT NOT NULL,
                    PRIMARY KEY (site, kind, id)
                )",
            )
            .execute(&pool)
            .await
            .unwrap();
            let site = Uuid::new_v4();
            for (kind, id) in [
                ("content", "draft-10"),
                ("content", "draft-2"),
                ("charter", "charter-3"),
                ("charter_sign", "charter-sign-4"),
            ] {
                sqlx::query(
                    "INSERT INTO adk_website_drafts (site, kind, id, value) VALUES (?, ?, ?, ?)",
                )
                .bind(site.to_string())
                .bind(kind)
                .bind(id)
                .bind(serde_json::json!({ "id": id }).to_string())
                .execute(&pool)
                .await
                .unwrap();
            }
            let store = harness.open().await;
            let listed = store.list(site, DraftKind::Content).await.unwrap();
            assert_eq!(
                listed,
                vec![
                    StoredRecord {
                        id: "draft-2".into(),
                        version: 1,
                        value: serde_json::json!({ "id": "draft-2" })
                    },
                    StoredRecord {
                        id: "draft-10".into(),
                        version: 1,
                        value: serde_json::json!({ "id": "draft-10" })
                    },
                ],
                "earlier records read back at version 1, in number order"
            );
            assert!(
                store
                    .replace(site, DraftKind::Content, "draft-2", &serde_json::json!(2), 1)
                    .await
                    .unwrap()
            );
            let next =
                store.create(site, DraftKind::Content, &serde_json::json!(11)).await.unwrap();
            assert_eq!(next.id, "draft-11", "new ids continue after the stored ones");
            let charter =
                store.create(site, DraftKind::Charter, &serde_json::json!(4)).await.unwrap();
            assert_eq!(charter.id, "charter-4");
            // Migrating again changes nothing.
            let again = harness.open().await;
            assert_eq!(again.list(site, DraftKind::Content).await.unwrap().len(), 3);
            let sign =
                again.create(site, DraftKind::CharterSign, &serde_json::json!(5)).await.unwrap();
            assert_eq!(sign.id, "charter-sign-5");
        }
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

    #[test]
    fn a_kind_reads_the_numbers_of_its_own_ids() {
        assert_eq!(DraftKind::Content.number("draft-12"), Some(12));
        assert_eq!(DraftKind::Charter.number("charter-sign-3"), None);
        assert_eq!(DraftKind::CharterSign.number("charter-sign-3"), Some(3));
        assert_eq!(DraftKind::Promotion.number("draft-1"), None);
    }
}
