//! Where standing rules are kept (spec 3.1 §7.5).

use std::fmt;
use std::path::{Path, PathBuf};

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::{Mutex, RwLock};

use super::rule::StandingRule;

/// Where a harness keeps standing rules (spec 3.1 §7.5).
///
/// The policy reads [`rules_for`](Self::rules_for) on every evaluation, so a revocation
/// applies to every call authorized after it commits (spec 3.1 §9.2 I4). Only an
/// authenticated human action grants a rule (I2).
///
/// # Example
///
/// ```rust,ignore
/// let rules = store.rules_for("adk-rust", "alice", "session-1").await?;
/// ```
#[async_trait]
pub trait StandingRuleStore: fmt::Debug + Send + Sync {
    /// The user-scoped rules of `user` in `app` and the session-scoped rules of `session`.
    async fn rules_for(&self, app: &str, user: &str, session: &str) -> Result<Vec<StandingRule>>;
    /// Stores `rule`, replacing a rule with the same id, and returns it.
    async fn grant(&self, rule: StandingRule) -> Result<StandingRule>;
    /// Removes the rule `id` of `app`. Returns whether it existed.
    async fn revoke(&self, app: &str, id: &str) -> Result<bool>;
    /// Every rule of `user` in `app`, of either scope.
    async fn list(&self, app: &str, user: &str) -> Result<Vec<StandingRule>>;

    /// Replaces the stored rule with `rule`'s id in `rule`'s app by `rule` while it is still
    /// stored, and returns whether it was, so a rule revoked meanwhile stays revoked (spec 3.3
    /// §8.1 "Extend").
    ///
    /// The default reads the owner's rules, then grants `rule` when its id is among them; a
    /// revocation between the two is undone. Every store of this crate replaces in one step.
    async fn replace(&self, rule: StandingRule) -> Result<bool> {
        let held = self.list(rule.scope.app_name(), rule.scope.user_id()).await?;
        if !held.iter().any(|existing| existing.id == rule.id) {
            return Ok(false);
        }
        self.grant(rule).await?;
        Ok(true)
    }
}

fn validate(rule: &StandingRule) -> Result<()> {
    if rule.id.trim().is_empty() || rule.tool.trim().is_empty() {
        return Err(AdkError::new(
            ErrorComponent::Agent,
            ErrorCategory::InvalidInput,
            "harness.rule.invalid",
            "a standing rule needs a non-empty id and tool pattern",
        ));
    }
    Ok(())
}

fn applies_to_session(rule: &StandingRule, app: &str, user: &str, session: &str) -> bool {
    rule.scope.app_name() == app
        && rule.scope.user_id() == user
        && rule.scope.session_id().is_none_or(|own| own == session)
}

/// A [`StandingRuleStore`] in process memory, the builder default (spec 3.1 §7.5).
///
/// # Example
///
/// ```rust
/// let store = adk_harness::InMemoryStandingRuleStore::new();
/// # let _ = store;
/// ```
#[derive(Debug, Default)]
pub struct InMemoryStandingRuleStore {
    rules: RwLock<Vec<StandingRule>>,
}

impl InMemoryStandingRuleStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl StandingRuleStore for InMemoryStandingRuleStore {
    async fn rules_for(&self, app: &str, user: &str, session: &str) -> Result<Vec<StandingRule>> {
        Ok(self
            .rules
            .read()
            .await
            .iter()
            .filter(|rule| applies_to_session(rule, app, user, session))
            .cloned()
            .collect())
    }

    async fn grant(&self, rule: StandingRule) -> Result<StandingRule> {
        validate(&rule)?;
        let mut rules = self.rules.write().await;
        rules.retain(|existing| existing.id != rule.id);
        rules.push(rule.clone());
        Ok(rule)
    }

    async fn revoke(&self, app: &str, id: &str) -> Result<bool> {
        let mut rules = self.rules.write().await;
        let before = rules.len();
        rules.retain(|rule| !(rule.scope.app_name() == app && rule.id == id));
        Ok(rules.len() != before)
    }

    async fn replace(&self, rule: StandingRule) -> Result<bool> {
        validate(&rule)?;
        let mut rules = self.rules.write().await;
        let held = rules.iter_mut().find(|existing| {
            existing.scope.app_name() == rule.scope.app_name() && existing.id == rule.id
        });
        Ok(held.map(|existing| *existing = rule).is_some())
    }

    async fn list(&self, app: &str, user: &str) -> Result<Vec<StandingRule>> {
        Ok(self
            .rules
            .read()
            .await
            .iter()
            .filter(|rule| rule.scope.app_name() == app && rule.scope.user_id() == user)
            .cloned()
            .collect())
    }
}

/// A [`StandingRuleStore`] of one JSON file per user:
/// `~/.adk/approvals/<app>/<sha256(user)>.json` (spec 3.1 §7.5).
///
/// Files are replaced atomically by rename and are `0600` on Unix. Entries this release
/// cannot parse — including rules with unknown fields — are ignored for matching and kept
/// on rewrite (spec 3.1 §9.2 I16).
///
/// # Example
///
/// ```rust
/// let dir = tempfile::tempdir().unwrap();
/// let store = adk_harness::FileStandingRuleStore::new(dir.path());
/// # let _ = store;
/// ```
#[derive(Debug)]
pub struct FileStandingRuleStore {
    root: PathBuf,
    write: Mutex<()>,
}

impl FileStandingRuleStore {
    /// A store under `root` (the directory holding one directory per app).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), write: Mutex::new(()) }
    }

    /// A store under `~/.adk/approvals`.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when the home directory cannot be determined.
    pub fn in_home() -> Result<Self> {
        Ok(Self::new(crate::fsutil::adk_home()?.join("approvals")))
    }

    fn app_dir(&self, app: &str) -> PathBuf {
        let safe = !app.is_empty()
            && app != "."
            && app != ".."
            && app.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if safe { self.root.join(app) } else { self.root.join(crate::fsutil::sha256_hex(app)) }
    }

    fn user_file(&self, app: &str, user: &str) -> PathBuf {
        self.app_dir(app).join(format!("{}.json", crate::fsutil::sha256_hex(user)))
    }

    async fn read_entries(path: &Path) -> Result<Vec<Value>> {
        let Some(bytes) = crate::fsutil::read_optional(path).await? else {
            return Ok(Vec::new());
        };
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Array(entries)) => Ok(entries),
            _ => {
                tracing::warn!(path = %path.display(), "standing rule file is not a JSON array; ignoring it");
                Ok(Vec::new())
            }
        }
    }

    fn parse(entries: &[Value]) -> Vec<StandingRule> {
        entries
            .iter()
            .filter_map(|entry| match serde_json::from_value::<StandingRule>(entry.clone()) {
                Ok(rule) => Some(rule),
                Err(error) => {
                    tracing::warn!(error = %error, "ignoring a standing rule this release cannot parse");
                    None
                }
            })
            .collect()
    }

    async fn write_entries(path: &Path, entries: &[Value]) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(entries)
            .map_err(|error| crate::fsutil::serialize_error("standing rules", &error))?;
        crate::fsutil::write_atomic_private(path, &bytes).await
    }
}

fn entry_id(entry: &Value) -> Option<&str> {
    entry.get("id").and_then(Value::as_str)
}

#[async_trait]
impl StandingRuleStore for FileStandingRuleStore {
    async fn rules_for(&self, app: &str, user: &str, session: &str) -> Result<Vec<StandingRule>> {
        let entries = Self::read_entries(&self.user_file(app, user)).await?;
        Ok(Self::parse(&entries)
            .into_iter()
            .filter(|rule| applies_to_session(rule, app, user, session))
            .collect())
    }

    async fn grant(&self, rule: StandingRule) -> Result<StandingRule> {
        validate(&rule)?;
        let _guard = self.write.lock().await;
        let path = self.user_file(rule.scope.app_name(), rule.scope.user_id());
        let mut entries = Self::read_entries(&path).await?;
        entries.retain(|entry| entry_id(entry) != Some(rule.id.as_str()));
        entries.push(
            serde_json::to_value(&rule)
                .map_err(|error| crate::fsutil::serialize_error("standing rule", &error))?,
        );
        Self::write_entries(&path, &entries).await?;
        Ok(rule)
    }

    async fn revoke(&self, app: &str, id: &str) -> Result<bool> {
        let _guard = self.write.lock().await;
        let dir = self.app_dir(app);
        let mut listing = match tokio::fs::read_dir(&dir).await {
            Ok(listing) => listing,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(AdkError::new(
                    ErrorComponent::Agent,
                    ErrorCategory::Unavailable,
                    "harness.store.io",
                    format!("list {}: {error}", dir.display()),
                ));
            }
        };
        let mut removed = false;
        while let Ok(Some(file)) = listing.next_entry().await {
            let path = file.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let mut entries = Self::read_entries(&path).await?;
            let before = entries.len();
            entries.retain(|entry| entry_id(entry) != Some(id));
            if entries.len() != before {
                Self::write_entries(&path, &entries).await?;
                removed = true;
            }
        }
        Ok(removed)
    }

    async fn replace(&self, rule: StandingRule) -> Result<bool> {
        validate(&rule)?;
        let _guard = self.write.lock().await;
        let path = self.user_file(rule.scope.app_name(), rule.scope.user_id());
        let mut entries = Self::read_entries(&path).await?;
        let Some(entry) =
            entries.iter_mut().find(|entry| entry_id(entry) == Some(rule.id.as_str()))
        else {
            return Ok(false);
        };
        *entry = serde_json::to_value(&rule)
            .map_err(|error| crate::fsutil::serialize_error("standing rule", &error))?;
        Self::write_entries(&path, &entries).await?;
        Ok(true)
    }

    async fn list(&self, app: &str, user: &str) -> Result<Vec<StandingRule>> {
        let entries = Self::read_entries(&self.user_file(app, user)).await?;
        Ok(Self::parse(&entries)
            .into_iter()
            .filter(|rule| rule.scope.app_name() == app && rule.scope.user_id() == user)
            .collect())
    }
}

/// A [`StandingRuleStore`] in SQLite (spec 3.1 §7.1, feature `sqlite`).
///
/// Call [`migrate`](Self::migrate) once before use. The table is `adk_harness_rules`.
///
/// # Example
///
/// ```rust,ignore
/// let store = adk_harness::SqliteStandingRuleStore::new(pool);
/// store.migrate().await?;
/// ```
#[cfg(feature = "sqlite")]
#[derive(Debug, Clone)]
pub struct SqliteStandingRuleStore {
    pool: sqlx::SqlitePool,
    #[cfg(feature = "encrypted")]
    envelope: Option<crate::encrypted::StoreEnvelope>,
}

#[cfg(feature = "sqlite")]
impl SqliteStandingRuleStore {
    /// A store over `pool`.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self {
            pool,
            #[cfg(feature = "encrypted")]
            envelope: None,
        }
    }

    /// Seals every record under `keyring` (spec 3.1 §7.12).
    #[cfg(feature = "encrypted")]
    #[must_use]
    pub fn with_keyring(mut self, keyring: crate::encrypted::StoreKeyring) -> Self {
        self.envelope = Some(crate::encrypted::StoreEnvelope::new(keyring, "rules"));
        self
    }

    /// Re-seals every record a previous key sealed with the current key, seals records
    /// written before a key was configured, and returns how many rows changed (spec 3.1
    /// §7.12). Without a keyring, nothing changes.
    ///
    /// # Errors
    ///
    /// Returns `harness.store.decrypt_failed` when a row opens under no key the keyring
    /// holds, or the database error.
    #[cfg(feature = "encrypted")]
    pub async fn rewrap(&self) -> Result<u64> {
        let Some(envelope) = &self.envelope else {
            return Ok(0);
        };
        let rows: Vec<(String, String, String)> =
            sqlx::query_as("SELECT app_name, id, rule FROM adk_harness_rules")
                .fetch_all(&self.pool)
                .await
                .map_err(crate::fsutil::db_error)?;
        let mut changed = 0;
        for (app, id, text) in rows {
            if let Some(resealed) = envelope.reseal_or_seal(&[&app, &id], &text)? {
                sqlx::query("UPDATE adk_harness_rules SET rule = ? WHERE app_name = ? AND id = ?")
                    .bind(resealed)
                    .bind(&app)
                    .bind(&id)
                    .execute(&self.pool)
                    .await
                    .map_err(crate::fsutil::db_error)?;
                changed += 1;
            }
        }
        Ok(changed)
    }

    #[cfg(feature = "encrypted")]
    fn encode(&self, address: &[&str], text: String) -> Result<String> {
        crate::encrypted::encode_row(self.envelope.as_ref(), address, text)
    }

    #[cfg(not(feature = "encrypted"))]
    fn encode(&self, _address: &[&str], text: String) -> Result<String> {
        Ok(text)
    }

    #[cfg(feature = "encrypted")]
    fn decode(&self, address: &[&str], text: String) -> Result<String> {
        crate::encrypted::decode_row(self.envelope.as_ref(), "rules", address, text)
    }

    #[cfg(not(feature = "encrypted"))]
    fn decode(&self, _address: &[&str], text: String) -> Result<String> {
        Ok(text)
    }

    /// Creates the store's table if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns the database error when the statement fails.
    pub async fn migrate(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS adk_harness_rules (\
                app_name TEXT NOT NULL, id TEXT NOT NULL, user_id TEXT NOT NULL, \
                session_id TEXT, rule TEXT NOT NULL, PRIMARY KEY (app_name, id))",
        )
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }

    fn parse(&self, app: &str, rows: Vec<(String, String)>) -> Result<Vec<StandingRule>> {
        let mut rules = Vec::with_capacity(rows.len());
        for (id, text) in rows {
            let text = self.decode(&[app, &id], text)?;
            match serde_json::from_str::<StandingRule>(&text) {
                Ok(rule) => rules.push(rule),
                Err(error) => {
                    tracing::warn!(error = %error, "ignoring a standing rule this release cannot parse");
                }
            }
        }
        Ok(rules)
    }
}

#[cfg(feature = "sqlite")]
#[async_trait]
impl StandingRuleStore for SqliteStandingRuleStore {
    async fn rules_for(&self, app: &str, user: &str, session: &str) -> Result<Vec<StandingRule>> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT id, rule FROM adk_harness_rules WHERE app_name = ? AND user_id = ? \
             AND (session_id IS NULL OR session_id = ?) ORDER BY rowid",
        )
        .bind(app)
        .bind(user)
        .bind(session)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(self
            .parse(app, rows)?
            .into_iter()
            .filter(|rule| applies_to_session(rule, app, user, session))
            .collect())
    }

    async fn grant(&self, rule: StandingRule) -> Result<StandingRule> {
        validate(&rule)?;
        let text = serde_json::to_string(&rule)
            .map_err(|error| crate::fsutil::serialize_error("standing rule", &error))?;
        let text = self.encode(&[rule.scope.app_name(), &rule.id], text)?;
        sqlx::query(
            "INSERT INTO adk_harness_rules (app_name, id, user_id, session_id, rule) VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT (app_name, id) DO UPDATE SET user_id = excluded.user_id, \
             session_id = excluded.session_id, rule = excluded.rule",
        )
        .bind(rule.scope.app_name())
        .bind(&rule.id)
        .bind(rule.scope.user_id())
        .bind(rule.scope.session_id())
        .bind(text)
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(rule)
    }

    async fn revoke(&self, app: &str, id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM adk_harness_rules WHERE app_name = ? AND id = ?")
            .bind(app)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(crate::fsutil::db_error)?;
        Ok(result.rows_affected() > 0)
    }

    async fn replace(&self, rule: StandingRule) -> Result<bool> {
        validate(&rule)?;
        let text = serde_json::to_string(&rule)
            .map_err(|error| crate::fsutil::serialize_error("standing rule", &error))?;
        let text = self.encode(&[rule.scope.app_name(), &rule.id], text)?;
        let result = sqlx::query(
            "UPDATE adk_harness_rules SET user_id = ?, session_id = ?, rule = ? \
             WHERE app_name = ? AND id = ?",
        )
        .bind(rule.scope.user_id())
        .bind(rule.scope.session_id())
        .bind(text)
        .bind(rule.scope.app_name())
        .bind(&rule.id)
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(result.rows_affected() > 0)
    }

    async fn list(&self, app: &str, user: &str) -> Result<Vec<StandingRule>> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT id, rule FROM adk_harness_rules WHERE app_name = ? AND user_id = ? ORDER BY rowid",
        )
        .bind(app)
        .bind(user)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        self.parse(app, rows)
    }
}

/// Parses stored rule JSON, ignoring entries this release cannot parse (spec 3.1 §9.2 I16).
#[cfg(any(feature = "postgres", feature = "redis"))]
fn parse_rule_texts(texts: impl IntoIterator<Item = String>) -> Vec<StandingRule> {
    texts
        .into_iter()
        .filter_map(|text| match serde_json::from_str::<StandingRule>(&text) {
            Ok(rule) => Some(rule),
            Err(error) => {
                tracing::warn!(error = %error, "ignoring a standing rule this release cannot parse");
                None
            }
        })
        .collect()
}

/// How many entries a shared store's rule cache keeps before it starts over.
#[cfg(any(feature = "postgres", feature = "redis"))]
const RULE_CACHE_ENTRIES: usize = 1024;

/// Rules a shared store loaded, each tagged with the version of its app's rules at the
/// time (spec 3.1 §7.5).
///
/// Every grant and revocation increments the app's version in the same transaction or
/// script that changes the rule. A read compares the stored version first and reloads on
/// any difference, so a change committed through any replica is seen at that replica's
/// next evaluation (spec 3.1 §9.2 I4); the version is read before the rules, so an entry
/// is never older than its tag.
#[cfg(any(feature = "postgres", feature = "redis"))]
#[derive(Debug)]
struct RuleCache<K> {
    entries: RwLock<std::collections::HashMap<K, VersionedRules>>,
}

/// Rules and the version of their app's rules they were loaded at.
#[cfg(any(feature = "postgres", feature = "redis"))]
type VersionedRules = (i64, std::sync::Arc<Vec<StandingRule>>);

#[cfg(any(feature = "postgres", feature = "redis"))]
impl<K: Eq + std::hash::Hash> RuleCache<K> {
    fn new() -> Self {
        Self { entries: RwLock::new(std::collections::HashMap::new()) }
    }

    async fn get(&self, key: &K, version: i64) -> Option<std::sync::Arc<Vec<StandingRule>>> {
        let entries = self.entries.read().await;
        entries.get(key).filter(|(at, _)| *at == version).map(|(_, rules)| rules.clone())
    }

    async fn put(&self, key: K, version: i64, rules: std::sync::Arc<Vec<StandingRule>>) {
        let mut entries = self.entries.write().await;
        if entries.len() >= RULE_CACHE_ENTRIES && !entries.contains_key(&key) {
            entries.clear();
        }
        entries.insert(key, (version, rules));
    }

    async fn forget(&self, matches: impl Fn(&K) -> bool) {
        self.entries.write().await.retain(|key, _| !matches(key));
    }
}

/// Migrations of [`PostgresStandingRuleStore`], recorded in `_adk_harness_rule_migrations`.
#[cfg(feature = "postgres")]
const POSTGRES_RULE_MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create standing rule tables",
    "CREATE TABLE IF NOT EXISTS adk_harness_rules (\
        seq BIGINT GENERATED ALWAYS AS IDENTITY, app_name TEXT NOT NULL, id TEXT NOT NULL, \
        user_id TEXT NOT NULL, session_id TEXT, rule TEXT NOT NULL, \
        PRIMARY KEY (app_name, id)); \
     CREATE INDEX IF NOT EXISTS adk_harness_rules_by_user \
        ON adk_harness_rules (app_name, user_id, seq); \
     CREATE TABLE IF NOT EXISTS adk_harness_rule_versions (\
        app_name TEXT PRIMARY KEY, version BIGINT NOT NULL);",
)];

/// A [`StandingRuleStore`] in PostgreSQL, shared by every replica of a served harness
/// (spec 3.1 §7.5, §13 Q2; feature `postgres`).
///
/// Rules live in `adk_harness_rules`, keyed as the SQLite store's are by (`app_name`,
/// `id`) with the owner's `user_id` and the `session_id` of a session-scoped rule beside
/// them; `adk_harness_rule_versions` holds one version per app.
///
/// [`rules_for`](StandingRuleStore::rules_for) is the policy's hot path: it reads the
/// app's version and answers from rules it loaded at that version, so an unchanged app
/// costs one primary-key lookup. [`grant`](StandingRuleStore::grant) and
/// [`revoke`](StandingRuleStore::revoke) increment the version in the transaction that
/// changes the rule, so a revocation committed through any replica is refused on every
/// replica's next evaluation (spec 3.1 §9.2 I4). Expiry and Deny-beats-Allow are the
/// matcher's, as for every store. [`migrate`](Self::migrate) records its steps in
/// `_adk_harness_rule_migrations` under an advisory lock.
///
/// # Example
///
/// ```rust,no_run
/// use adk_harness::{PostgresStandingRuleStore, StandingRuleStore};
///
/// # async fn example() -> adk_core::Result<()> {
/// let store = PostgresStandingRuleStore::connect("postgres://localhost/app").await?;
/// store.migrate().await?;
/// let rules = store.rules_for("adk-rust", "alice", "session-1").await?;
/// # let _ = rules;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "postgres")]
#[derive(Debug, Clone)]
pub struct PostgresStandingRuleStore {
    pool: sqlx::PgPool,
    cache: std::sync::Arc<RuleCache<(String, String)>>,
}

#[cfg(feature = "postgres")]
impl PostgresStandingRuleStore {
    /// A store over `pool`, such as the one the session service and the approval store use.
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool, cache: std::sync::Arc::new(RuleCache::new()) }
    }

    /// Connects to `database_url` with a pool of its own.
    ///
    /// # Errors
    ///
    /// Returns `harness.store.database` when the server cannot be reached.
    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool = sqlx::PgPool::connect(database_url).await.map_err(crate::fsutil::db_error)?;
        Ok(Self::new(pool))
    }

    /// Creates or upgrades the store's tables.
    ///
    /// # Errors
    ///
    /// Returns the database error when a statement fails, or a version mismatch when the
    /// database was migrated by a newer release.
    pub async fn migrate(&self) -> Result<()> {
        crate::fsutil::migrate_postgres(
            &self.pool,
            "_adk_harness_rule_migrations",
            POSTGRES_RULE_MIGRATIONS,
        )
        .await
    }

    /// Every rule of `user` in `app`, from the cache while the app's version is unchanged.
    async fn user_rules(&self, app: &str, user: &str) -> Result<std::sync::Arc<Vec<StandingRule>>> {
        let version: Option<(i64,)> =
            sqlx::query_as("SELECT version FROM adk_harness_rule_versions WHERE app_name = $1")
                .bind(app)
                .fetch_optional(&self.pool)
                .await
                .map_err(crate::fsutil::db_error)?;
        let version = version.map_or(0, |(version,)| version);
        let key = (app.to_string(), user.to_string());
        if let Some(rules) = self.cache.get(&key, version).await {
            return Ok(rules);
        }
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT rule FROM adk_harness_rules WHERE app_name = $1 AND user_id = $2 ORDER BY seq",
        )
        .bind(app)
        .bind(user)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        let rules = std::sync::Arc::new(parse_rule_texts(rows.into_iter().map(|(text,)| text)));
        self.cache.put(key, version, rules.clone()).await;
        Ok(rules)
    }
}

/// Increments `app`'s rule version; the row lock it takes orders concurrent changes.
#[cfg(feature = "postgres")]
const BUMP_RULE_VERSION: &str = "INSERT INTO adk_harness_rule_versions (app_name, version) \
     VALUES ($1, 1) ON CONFLICT (app_name) \
     DO UPDATE SET version = adk_harness_rule_versions.version + 1";

#[cfg(feature = "postgres")]
#[async_trait]
impl StandingRuleStore for PostgresStandingRuleStore {
    async fn rules_for(&self, app: &str, user: &str, session: &str) -> Result<Vec<StandingRule>> {
        Ok(self
            .user_rules(app, user)
            .await?
            .iter()
            .filter(|rule| applies_to_session(rule, app, user, session))
            .cloned()
            .collect())
    }

    async fn grant(&self, rule: StandingRule) -> Result<StandingRule> {
        validate(&rule)?;
        let text = serde_json::to_string(&rule)
            .map_err(|error| crate::fsutil::serialize_error("standing rule", &error))?;
        let app = rule.scope.app_name().to_string();
        let mut tx = self.pool.begin().await.map_err(crate::fsutil::db_error)?;
        sqlx::query(BUMP_RULE_VERSION)
            .bind(&app)
            .execute(&mut *tx)
            .await
            .map_err(crate::fsutil::db_error)?;
        sqlx::query(
            "INSERT INTO adk_harness_rules (app_name, id, user_id, session_id, rule) \
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT (app_name, id) DO UPDATE SET \
             user_id = excluded.user_id, session_id = excluded.session_id, rule = excluded.rule",
        )
        .bind(&app)
        .bind(&rule.id)
        .bind(rule.scope.user_id())
        .bind(rule.scope.session_id())
        .bind(text)
        .execute(&mut *tx)
        .await
        .map_err(crate::fsutil::db_error)?;
        tx.commit().await.map_err(crate::fsutil::db_error)?;
        self.cache.forget(|(cached, _)| *cached == app).await;
        Ok(rule)
    }

    async fn revoke(&self, app: &str, id: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await.map_err(crate::fsutil::db_error)?;
        sqlx::query(BUMP_RULE_VERSION)
            .bind(app)
            .execute(&mut *tx)
            .await
            .map_err(crate::fsutil::db_error)?;
        let deleted = sqlx::query("DELETE FROM adk_harness_rules WHERE app_name = $1 AND id = $2")
            .bind(app)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(crate::fsutil::db_error)?
            .rows_affected();
        if deleted == 0 {
            tx.rollback().await.map_err(crate::fsutil::db_error)?;
            return Ok(false);
        }
        tx.commit().await.map_err(crate::fsutil::db_error)?;
        self.cache.forget(|(cached, _)| cached == app).await;
        Ok(true)
    }

    async fn replace(&self, rule: StandingRule) -> Result<bool> {
        validate(&rule)?;
        let text = serde_json::to_string(&rule)
            .map_err(|error| crate::fsutil::serialize_error("standing rule", &error))?;
        let app = rule.scope.app_name().to_string();
        let mut tx = self.pool.begin().await.map_err(crate::fsutil::db_error)?;
        sqlx::query(BUMP_RULE_VERSION)
            .bind(&app)
            .execute(&mut *tx)
            .await
            .map_err(crate::fsutil::db_error)?;
        let updated = sqlx::query(
            "UPDATE adk_harness_rules SET user_id = $1, session_id = $2, rule = $3 \
             WHERE app_name = $4 AND id = $5",
        )
        .bind(rule.scope.user_id())
        .bind(rule.scope.session_id())
        .bind(text)
        .bind(&app)
        .bind(&rule.id)
        .execute(&mut *tx)
        .await
        .map_err(crate::fsutil::db_error)?
        .rows_affected();
        if updated == 0 {
            tx.rollback().await.map_err(crate::fsutil::db_error)?;
            return Ok(false);
        }
        tx.commit().await.map_err(crate::fsutil::db_error)?;
        self.cache.forget(|(cached, _)| *cached == app).await;
        Ok(true)
    }

    async fn list(&self, app: &str, user: &str) -> Result<Vec<StandingRule>> {
        Ok(self
            .user_rules(app, user)
            .await?
            .iter()
            .filter(|rule| rule.scope.app_name() == app && rule.scope.user_id() == user)
            .cloned()
            .collect())
    }
}

/// KEYS: version, rules, order, sequence. ARGV: rule id, rule JSON. Stores the rule,
/// keeps the position of a rule it replaces, and increments the app's version.
#[cfg(feature = "redis")]
const GRANT_SCRIPT: &str = r#"
redis.call('INCR', KEYS[1])
redis.call('HSET', KEYS[2], ARGV[1], ARGV[2])
if not redis.call('ZSCORE', KEYS[3], ARGV[1]) then
  redis.call('ZADD', KEYS[3], redis.call('INCR', KEYS[4]), ARGV[1])
end
return 1
"#;

/// KEYS: version, rules, order. ARGV: rule id. Returns 1 when the rule existed.
#[cfg(feature = "redis")]
const REVOKE_SCRIPT: &str = r#"
if redis.call('HDEL', KEYS[2], ARGV[1]) == 0 then return 0 end
redis.call('ZREM', KEYS[3], ARGV[1])
redis.call('INCR', KEYS[1])
return 1
"#;

/// KEYS: version, rules. ARGV: rule id, rule JSON. Replaces a stored rule and increments the
/// app's version; returns 0, changing nothing, when the rule is not stored.
#[cfg(feature = "redis")]
const REPLACE_SCRIPT: &str = r#"
if redis.call('HEXISTS', KEYS[2], ARGV[1]) == 0 then return 0 end
redis.call('HSET', KEYS[2], ARGV[1], ARGV[2])
redis.call('INCR', KEYS[1])
return 1
"#;

/// KEYS: version, rules, order. Returns the app's version, then its rules' JSON in grant
/// order, read atomically.
#[cfg(feature = "redis")]
const READ_RULES_SCRIPT: &str = r#"
local out = { redis.call('GET', KEYS[1]) or '0' }
for _, id in ipairs(redis.call('ZRANGE', KEYS[3], 0, -1)) do
  local rule = redis.call('HGET', KEYS[2], id)
  if rule then table.insert(out, rule) end
end
return out
"#;

/// A [`StandingRuleStore`] in Redis, shared by every replica of a served harness
/// (spec 3.1 §7.5, §13 Q2; feature `redis`).
///
/// Each app's keys share its hash tag, so they sit in one Redis Cluster slot and every
/// change runs as one atomic script:
///
/// | Key | Type | Contents |
/// |-----|------|----------|
/// | `<namespace>:{<app>}:rules` | Hash | Rule id → rule JSON |
/// | `<namespace>:{<app>}:rule-order` | Sorted set | Rule ids by first grant, so a replaced rule keeps its place |
/// | `<namespace>:{<app>}:rule-seq` | String | The grant counter that orders rules |
/// | `<namespace>:{<app>}:rule-version` | String | The app's version, incremented by every grant and revocation |
///
/// The namespace is `adk:harness` unless [`with_namespace`](Self::with_namespace) sets
/// another; the app segment is percent-encoded (`%`, `:`, `{`, `}`).
/// [`rules_for`](StandingRuleStore::rules_for) reads the app's version and answers from
/// the rules it loaded at that version, so an unchanged app costs one `GET`; a revocation
/// through any replica is refused on every replica's next evaluation (spec 3.1 §9.2 I4).
///
/// # Example
///
/// ```rust,no_run
/// use adk_harness::{RedisStandingRuleStore, StandingRuleStore};
///
/// # async fn example() -> adk_core::Result<()> {
/// let store = RedisStandingRuleStore::connect("redis://localhost:6379").await?;
/// store.migrate().await?;
/// let rules = store.rules_for("adk-rust", "alice", "session-1").await?;
/// # let _ = rules;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "redis")]
#[derive(Debug, Clone)]
pub struct RedisStandingRuleStore {
    client: fred::prelude::Client,
    namespace: String,
    cache: std::sync::Arc<RuleCache<String>>,
}

#[cfg(feature = "redis")]
impl RedisStandingRuleStore {
    /// A store over an initialized client, such as one shared with the mode store.
    pub fn from_client(client: fred::prelude::Client) -> Self {
        Self {
            client,
            namespace: crate::fsutil::REDIS_NAMESPACE.to_string(),
            cache: std::sync::Arc::new(RuleCache::new()),
        }
    }

    /// Connects to `url`.
    ///
    /// # Errors
    ///
    /// Returns `harness.store.database` when the server cannot be reached.
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
    /// Returns `harness.store.database` when the server rejects a script.
    pub async fn migrate(&self) -> Result<()> {
        use fred::prelude::LuaInterface as _;
        for script in [GRANT_SCRIPT, REVOKE_SCRIPT, REPLACE_SCRIPT, READ_RULES_SCRIPT] {
            let _: String = self
                .client
                .script_load(script)
                .await
                .map_err(|error| crate::fsutil::redis_error("load a script", &error))?;
        }
        Ok(())
    }

    /// The app's version, rules, order and sequence keys.
    fn keys(&self, app: &str) -> [String; 4] {
        let prefix = format!("{}:{{{}}}", self.namespace, crate::fsutil::redis_segment(app));
        [
            format!("{prefix}:rule-version"),
            format!("{prefix}:rules"),
            format!("{prefix}:rule-order"),
            format!("{prefix}:rule-seq"),
        ]
    }

    /// Every rule of `app`, from the cache while the app's version is unchanged.
    async fn app_rules(&self, app: &str) -> Result<std::sync::Arc<Vec<StandingRule>>> {
        use fred::prelude::{KeysInterface as _, LuaInterface as _};
        let [version_key, rules_key, order_key, _] = self.keys(app);
        let version: Option<i64> = self
            .client
            .get(&version_key)
            .await
            .map_err(|error| crate::fsutil::redis_error("read the rule version", &error))?;
        if let Some(rules) = self.cache.get(&app.to_string(), version.unwrap_or(0)).await {
            return Ok(rules);
        }
        let reply: Vec<String> = self
            .client
            .eval(READ_RULES_SCRIPT, vec![version_key, rules_key, order_key], Vec::<String>::new())
            .await
            .map_err(|error| crate::fsutil::redis_error("read standing rules", &error))?;
        let mut reply = reply.into_iter();
        let version = reply.next().and_then(|version| version.parse::<i64>().ok()).unwrap_or(0);
        let rules = std::sync::Arc::new(parse_rule_texts(reply));
        self.cache.put(app.to_string(), version, rules.clone()).await;
        Ok(rules)
    }
}

#[cfg(feature = "redis")]
#[async_trait]
impl StandingRuleStore for RedisStandingRuleStore {
    async fn rules_for(&self, app: &str, user: &str, session: &str) -> Result<Vec<StandingRule>> {
        Ok(self
            .app_rules(app)
            .await?
            .iter()
            .filter(|rule| applies_to_session(rule, app, user, session))
            .cloned()
            .collect())
    }

    async fn grant(&self, rule: StandingRule) -> Result<StandingRule> {
        use fred::prelude::LuaInterface as _;
        validate(&rule)?;
        let text = serde_json::to_string(&rule)
            .map_err(|error| crate::fsutil::serialize_error("standing rule", &error))?;
        let app = rule.scope.app_name();
        let _: i64 = self
            .client
            .eval(GRANT_SCRIPT, self.keys(app).to_vec(), vec![rule.id.clone(), text])
            .await
            .map_err(|error| crate::fsutil::redis_error("grant a standing rule", &error))?;
        self.cache.forget(|cached| cached == app).await;
        Ok(rule)
    }

    async fn revoke(&self, app: &str, id: &str) -> Result<bool> {
        use fred::prelude::LuaInterface as _;
        let [version_key, rules_key, order_key, _] = self.keys(app);
        let removed: i64 = self
            .client
            .eval(REVOKE_SCRIPT, vec![version_key, rules_key, order_key], vec![id.to_string()])
            .await
            .map_err(|error| crate::fsutil::redis_error("revoke a standing rule", &error))?;
        self.cache.forget(|cached| cached == app).await;
        Ok(removed == 1)
    }

    async fn replace(&self, rule: StandingRule) -> Result<bool> {
        use fred::prelude::LuaInterface as _;
        validate(&rule)?;
        let text = serde_json::to_string(&rule)
            .map_err(|error| crate::fsutil::serialize_error("standing rule", &error))?;
        let app = rule.scope.app_name();
        let [version_key, rules_key, _, _] = self.keys(app);
        let replaced: i64 = self
            .client
            .eval(REPLACE_SCRIPT, vec![version_key, rules_key], vec![rule.id.clone(), text])
            .await
            .map_err(|error| crate::fsutil::redis_error("replace a standing rule", &error))?;
        self.cache.forget(|cached| cached == app).await;
        Ok(replaced == 1)
    }

    async fn list(&self, app: &str, user: &str) -> Result<Vec<StandingRule>> {
        Ok(self
            .app_rules(app)
            .await?
            .iter()
            .filter(|rule| rule.scope.app_name() == app && rule.scope.user_id() == user)
            .cloned()
            .collect())
    }
}
