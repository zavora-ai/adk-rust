//! The per-session shell ledger behind unsandboxed retries (spec 3.1 §7.9).

use std::collections::HashMap;
use std::fmt;

use adk_core::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// One `bash` call, as the shell ledger records it (spec 3.1 §7.9).
///
/// # Example
///
/// ```rust
/// use adk_harness::ShellLedgerEntry;
///
/// let entry = ShellLedgerEntry::new("call-1", "cargo test", true, Some(101));
/// assert!(entry.failed());
/// assert!(entry.retried_by.is_none());
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellLedgerEntry {
    /// The call's function call ID.
    pub function_call_id: String,
    /// The command as run.
    pub command: String,
    /// Whether it ran under a sandbox.
    pub sandboxed: bool,
    /// The enforcer it ran under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforcer: Option<String>,
    /// Exit code; `None` when the command was killed or timed out.
    pub exit_code: Option<i32>,
    /// Whether stderr suggests the sandbox denied something.
    #[serde(default)]
    pub denial_suspected: bool,
    /// The call that retried it unsandboxed, once one has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retried_by: Option<String>,
    /// When it finished.
    pub at: DateTime<Utc>,
}

impl ShellLedgerEntry {
    /// An entry for a finished call.
    pub fn new(
        function_call_id: impl Into<String>,
        command: impl Into<String>,
        sandboxed: bool,
        exit_code: Option<i32>,
    ) -> Self {
        Self {
            function_call_id: function_call_id.into(),
            command: command.into(),
            sandboxed,
            enforcer: None,
            exit_code,
            denial_suspected: false,
            retried_by: None,
            at: Utc::now(),
        }
    }

    /// Whether the command failed: a non-zero exit, a kill or a timeout.
    pub fn failed(&self) -> bool {
        self.exit_code != Some(0)
    }

    /// Digest of the failed call an unsandboxed retry names: call ID, command, enforcer and
    /// exit code, as `sha256:<hex>` (spec 3.1 §9.3 `failureDigest`).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::ShellLedgerEntry;
    ///
    /// let one = ShellLedgerEntry::new("c1", "make", true, Some(2));
    /// let other = ShellLedgerEntry::new("c1", "make", true, Some(1));
    /// assert_ne!(one.digest(), other.digest());
    /// ```
    pub fn digest(&self) -> String {
        let failure = serde_json::json!({
            "functionCallId": self.function_call_id,
            "command": self.command,
            "sandboxed": self.sandboxed,
            "enforcer": self.enforcer,
            "exitCode": self.exit_code,
            "denialSuspected": self.denial_suspected,
        });
        format!("sha256:{}", crate::fsutil::sha256_hex(&failure.to_string()))
    }
}

/// Where the harness records each session's `bash` calls (spec 3.1 §7.9).
///
/// An unsandboxed retry is authorized only against a failed sandboxed call recorded here,
/// and [`mark_retried`](Self::mark_retried) lets exactly one retry consume it
/// (spec 3.1 §9.2 I25).
///
/// # Example
///
/// ```rust
/// use adk_harness::{InMemoryShellLedger, ShellLedger, ShellLedgerEntry};
///
/// # futures::executor::block_on(async {
/// let ledger = InMemoryShellLedger::new();
/// ledger.record("app", "u", "s", ShellLedgerEntry::new("c1", "make", true, Some(2))).await?;
/// assert!(ledger.mark_retried("app", "u", "s", "c1", "c2").await?);
/// assert!(!ledger.mark_retried("app", "u", "s", "c1", "c3").await?);
/// # Ok::<(), adk_core::AdkError>(())
/// # }).unwrap();
/// ```
#[async_trait]
pub trait ShellLedger: fmt::Debug + Send + Sync {
    /// Records a finished call, replacing an earlier entry with the same call ID.
    async fn record(
        &self,
        app: &str,
        user: &str,
        session: &str,
        entry: ShellLedgerEntry,
    ) -> Result<()>;
    /// The entry for `function_call_id`, if recorded.
    async fn get(
        &self,
        app: &str,
        user: &str,
        session: &str,
        function_call_id: &str,
    ) -> Result<Option<ShellLedgerEntry>>;
    /// Marks the entry as retried by `by`. Returns `true` only for the first retry of a
    /// recorded entry.
    async fn mark_retried(
        &self,
        app: &str,
        user: &str,
        session: &str,
        function_call_id: &str,
        by: &str,
    ) -> Result<bool>;
}

type LedgerKey = (String, String, String, String);

/// A [`ShellLedger`] in process memory, the default (spec 3.1 §7.9).
///
/// # Example
///
/// ```rust
/// let ledger = adk_harness::InMemoryShellLedger::new();
/// # let _ = ledger;
/// ```
#[derive(Debug, Default)]
pub struct InMemoryShellLedger {
    entries: RwLock<HashMap<LedgerKey, ShellLedgerEntry>>,
}

impl InMemoryShellLedger {
    /// An empty ledger.
    pub fn new() -> Self {
        Self::default()
    }
}

fn key(app: &str, user: &str, session: &str, call: &str) -> LedgerKey {
    (app.to_string(), user.to_string(), session.to_string(), call.to_string())
}

#[async_trait]
impl ShellLedger for InMemoryShellLedger {
    async fn record(
        &self,
        app: &str,
        user: &str,
        session: &str,
        entry: ShellLedgerEntry,
    ) -> Result<()> {
        self.entries.write().await.insert(key(app, user, session, &entry.function_call_id), entry);
        Ok(())
    }

    async fn get(
        &self,
        app: &str,
        user: &str,
        session: &str,
        function_call_id: &str,
    ) -> Result<Option<ShellLedgerEntry>> {
        Ok(self.entries.read().await.get(&key(app, user, session, function_call_id)).cloned())
    }

    async fn mark_retried(
        &self,
        app: &str,
        user: &str,
        session: &str,
        function_call_id: &str,
        by: &str,
    ) -> Result<bool> {
        let mut entries = self.entries.write().await;
        match entries.get_mut(&key(app, user, session, function_call_id)) {
            Some(entry) if entry.retried_by.is_none() => {
                entry.retried_by = Some(by.to_string());
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

/// A [`ShellLedger`] in SQLite (spec 3.1 §7.1, feature `sqlite`).
///
/// Call [`migrate`](Self::migrate) once before use. The table is
/// `adk_harness_shell_ledger`.
///
/// # Example
///
/// ```rust,ignore
/// let ledger = adk_harness::SqliteShellLedger::new(pool);
/// ledger.migrate().await?;
/// ```
#[cfg(feature = "sqlite")]
#[derive(Debug, Clone)]
pub struct SqliteShellLedger {
    pool: sqlx::SqlitePool,
    #[cfg(feature = "encrypted")]
    envelope: Option<crate::encrypted::StoreEnvelope>,
}

#[cfg(feature = "sqlite")]
impl SqliteShellLedger {
    /// A ledger over `pool`.
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
        self.envelope = Some(crate::encrypted::StoreEnvelope::new(keyring, "shell-ledger"));
        self
    }

    /// Re-seals every record a previous key sealed with the current key and returns how
    /// many rows changed (spec 3.1 §7.12). Without a keyring, nothing changes.
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
        let rows: Vec<(String, String, String, String, String)> = sqlx::query_as("SELECT app_name, user_id, session_id, function_call_id, entry FROM adk_harness_shell_ledger")
            .fetch_all(&self.pool)
            .await
            .map_err(crate::fsutil::db_error)?;
        let mut changed = 0;
        for (app, user, session, call, text) in rows {
            if let Some(resealed) = envelope.reseal(&[&app, &user, &session, &call], &text)? {
                sqlx::query("UPDATE adk_harness_shell_ledger SET entry = ? WHERE app_name = ? AND user_id = ? AND session_id = ? AND function_call_id = ?")
                    .bind(resealed)
                    .bind(&app)
                    .bind(&user)
                    .bind(&session)
                    .bind(&call)
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
        crate::encrypted::decode_row(self.envelope.as_ref(), "shell-ledger", address, text)
    }

    #[cfg(not(feature = "encrypted"))]
    fn decode(&self, _address: &[&str], text: String) -> Result<String> {
        Ok(text)
    }

    /// Creates the ledger's table if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns the database error when the statement fails.
    pub async fn migrate(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS adk_harness_shell_ledger (\
                app_name TEXT NOT NULL, user_id TEXT NOT NULL, session_id TEXT NOT NULL, \
                function_call_id TEXT NOT NULL, entry TEXT NOT NULL, retried_by TEXT, \
                PRIMARY KEY (app_name, user_id, session_id, function_call_id))",
        )
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }
}

#[cfg(feature = "sqlite")]
#[async_trait]
impl ShellLedger for SqliteShellLedger {
    async fn record(
        &self,
        app: &str,
        user: &str,
        session: &str,
        entry: ShellLedgerEntry,
    ) -> Result<()> {
        let text = serde_json::to_string(&entry)
            .map_err(|error| crate::fsutil::serialize_error("shell ledger entry", &error))?;
        let text = self.encode(&[app, user, session, &entry.function_call_id], text)?;
        sqlx::query(
            "INSERT INTO adk_harness_shell_ledger \
             (app_name, user_id, session_id, function_call_id, entry, retried_by) VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT (app_name, user_id, session_id, function_call_id) \
             DO UPDATE SET entry = excluded.entry",
        )
        .bind(app)
        .bind(user)
        .bind(session)
        .bind(&entry.function_call_id)
        .bind(text)
        .bind(entry.retried_by.as_deref())
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }

    async fn get(
        &self,
        app: &str,
        user: &str,
        session: &str,
        function_call_id: &str,
    ) -> Result<Option<ShellLedgerEntry>> {
        let row: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT entry, retried_by FROM adk_harness_shell_ledger \
             WHERE app_name = ? AND user_id = ? AND session_id = ? AND function_call_id = ?",
        )
        .bind(app)
        .bind(user)
        .bind(session)
        .bind(function_call_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        let Some((text, retried_by)) = row else {
            return Ok(None);
        };
        let text = self.decode(&[app, user, session, function_call_id], text)?;
        Ok(serde_json::from_str::<ShellLedgerEntry>(&text).ok().map(|mut entry| {
            entry.retried_by = retried_by;
            entry
        }))
    }

    async fn mark_retried(
        &self,
        app: &str,
        user: &str,
        session: &str,
        function_call_id: &str,
        by: &str,
    ) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE adk_harness_shell_ledger SET retried_by = ? \
             WHERE app_name = ? AND user_id = ? AND session_id = ? AND function_call_id = ? \
             AND retried_by IS NULL",
        )
        .bind(by)
        .bind(app)
        .bind(user)
        .bind(session)
        .bind(function_call_id)
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(result.rows_affected() == 1)
    }
}
