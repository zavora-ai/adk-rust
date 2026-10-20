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
}

#[cfg(feature = "sqlite")]
impl SqliteStandingRuleStore {
    /// A store over `pool`.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
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

    fn parse(rows: Vec<(String,)>) -> Vec<StandingRule> {
        rows.into_iter()
            .filter_map(|(text,)| match serde_json::from_str::<StandingRule>(&text) {
                Ok(rule) => Some(rule),
                Err(error) => {
                    tracing::warn!(error = %error, "ignoring a standing rule this release cannot parse");
                    None
                }
            })
            .collect()
    }
}

#[cfg(feature = "sqlite")]
#[async_trait]
impl StandingRuleStore for SqliteStandingRuleStore {
    async fn rules_for(&self, app: &str, user: &str, session: &str) -> Result<Vec<StandingRule>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT rule FROM adk_harness_rules WHERE app_name = ? AND user_id = ? \
             AND (session_id IS NULL OR session_id = ?) ORDER BY rowid",
        )
        .bind(app)
        .bind(user)
        .bind(session)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(Self::parse(rows)
            .into_iter()
            .filter(|rule| applies_to_session(rule, app, user, session))
            .collect())
    }

    async fn grant(&self, rule: StandingRule) -> Result<StandingRule> {
        validate(&rule)?;
        let text = serde_json::to_string(&rule)
            .map_err(|error| crate::fsutil::serialize_error("standing rule", &error))?;
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

    async fn list(&self, app: &str, user: &str) -> Result<Vec<StandingRule>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT rule FROM adk_harness_rules WHERE app_name = ? AND user_id = ? ORDER BY rowid",
        )
        .bind(app)
        .bind(user)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(Self::parse(rows))
    }
}
