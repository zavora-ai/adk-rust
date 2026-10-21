//! Harness modes and the stores that hold them (spec 3.1 §7.4).
//!
//! A session's mode lives in a harness-owned [`ModeStore`]. Session state holds a
//! display mirror under [`MODE_STATE_KEY`] that the policy never reads, so no tool,
//! client `stateDelta` or delegate merge can change the mode (spec 3.1 §9.2 I1).

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;

/// Session-state key of the mode's display mirror (spec 3.1 §7.4).
///
/// The policy never reads it; it exists so UIs can show the mode from
/// `event.actions.state_delta`. The mode itself is the [`ModeStore`]'s, which
/// `Harness::mode` reads: a run re-asserts the mirror from the store when it starts, and
/// any value an event of the run writes under this key is replaced with the store's
/// (spec 3.1 §6 D-3).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::MODE_STATE_KEY, "harness:mode");
/// ```
pub const MODE_STATE_KEY: &str = "harness:mode";

/// The mirror value the store's record of a session warrants: its mode, revision and
/// source, or the default mode when there is no record. `None` when the store cannot be
/// read, so nothing is written from a guess.
pub(crate) async fn mode_mirror(
    store: &dyn ModeStore,
    default_mode: HarnessMode,
    (app, user, session): (&str, &str, &str),
) -> Option<Value> {
    match store.get(app, user, session).await {
        Ok(Some(record)) => Some(
            serde_json::json!({ "mode": record.mode, "revision": record.revision, "setBy": record.set_by }),
        ),
        Ok(None) => Some(
            serde_json::json!({ "mode": default_mode, "revision": 0, "setBy": ModeSource::Default }),
        ),
        Err(_) => None,
    }
}

/// What the harness lets a session do without asking (spec 3.1 §7.4).
///
/// Ordered from strictest to loosest: `ReadOnly` ⪰ `Plan` ⪰ `Ask` ⪰ `Auto`. `Ask` is the
/// default. An unparseable mode is `ReadOnly` ([`HarnessMode::parse_lenient`]).
///
/// # Example
///
/// ```rust
/// use adk_harness::HarnessMode;
///
/// assert_eq!(HarnessMode::default(), HarnessMode::Ask);
/// assert!(HarnessMode::Plan.is_stricter_than(HarnessMode::Ask));
/// assert_eq!(HarnessMode::parse_lenient("yolo"), HarnessMode::ReadOnly);
/// assert_eq!("read-only".parse::<HarnessMode>().unwrap(), HarnessMode::ReadOnly);
/// ```
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessMode {
    /// Reads and todos only.
    ReadOnly,
    /// Reads, todos and `exit_plan_mode`; nothing that changes the workspace.
    Plan,
    /// Edits and the shell need approval unless a standing rule allows them.
    #[default]
    Ask,
    /// Edits run without approval; the host shell still needs approval.
    Auto,
}

impl HarnessMode {
    /// Every mode, strictest first.
    pub const ALL: [HarnessMode; 4] =
        [HarnessMode::ReadOnly, HarnessMode::Plan, HarnessMode::Ask, HarnessMode::Auto];

    /// The wire name: `read_only`, `plan`, `ask` or `auto`.
    ///
    /// # Example
    ///
    /// ```rust
    /// assert_eq!(adk_harness::HarnessMode::ReadOnly.as_str(), "read_only");
    /// ```
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Plan => "plan",
            Self::Ask => "ask",
            Self::Auto => "auto",
        }
    }

    /// Rank in the mode lattice: higher is stricter.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::HarnessMode;
    ///
    /// assert!(HarnessMode::ReadOnly.strictness() > HarnessMode::Auto.strictness());
    /// ```
    pub fn strictness(self) -> u8 {
        match self {
            Self::ReadOnly => 3,
            Self::Plan => 2,
            Self::Ask => 1,
            Self::Auto => 0,
        }
    }

    /// Whether `self` permits strictly less than `other`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::HarnessMode;
    ///
    /// assert!(HarnessMode::Ask.is_stricter_than(HarnessMode::Auto));
    /// assert!(!HarnessMode::Ask.is_stricter_than(HarnessMode::Ask));
    /// ```
    pub fn is_stricter_than(self, other: Self) -> bool {
        self.strictness() > other.strictness()
    }

    /// Parses a mode name, reading anything unknown as `ReadOnly` (spec 3.1 §9.2 I16).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::HarnessMode;
    ///
    /// assert_eq!(HarnessMode::parse_lenient("auto"), HarnessMode::Auto);
    /// assert_eq!(HarnessMode::parse_lenient("AUTO "), HarnessMode::ReadOnly);
    /// ```
    pub fn parse_lenient(value: &str) -> Self {
        value.parse().unwrap_or(Self::ReadOnly)
    }
}

impl fmt::Display for HarnessMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for HarnessMode {
    type Err = AdkError;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "read_only" | "read-only" | "readonly" => Ok(Self::ReadOnly),
            "plan" => Ok(Self::Plan),
            "ask" => Ok(Self::Ask),
            "auto" => Ok(Self::Auto),
            other => Err(AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::InvalidInput,
                "harness.mode.unknown",
                format!("unknown harness mode '{other}'; use read-only, plan, ask or auto"),
            )),
        }
    }
}

/// Who set a [`ModeRecord`] (spec 3.1 §7.4).
///
/// # Example
///
/// ```rust
/// use adk_harness::ModeSource;
///
/// assert_eq!(serde_json::to_value(ModeSource::PlanApproval).unwrap(), "plan_approval");
/// ```
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModeSource {
    /// The harness default; no record was ever written.
    Default,
    /// A person, through `Harness::set_mode`.
    User,
    /// An approved `exit_plan_mode` call.
    PlanApproval,
    /// The operator's configuration.
    Operator,
    /// The harness tightened the mode itself, for example on a detected loop.
    SystemTighten,
}

/// A session's mode as a [`ModeStore`] keeps it (spec 3.1 §7.4).
///
/// # Example
///
/// ```rust
/// use adk_harness::{HarnessMode, ModeRecord, ModeSource};
///
/// let record = ModeRecord::new(HarnessMode::Plan, ModeSource::User);
/// let json = serde_json::to_value(&record).unwrap();
/// assert_eq!(json["mode"], "plan");
/// assert_eq!(json["setBy"], "user");
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeRecord {
    /// The mode.
    pub mode: HarnessMode,
    /// Incremented on every change.
    pub revision: u64,
    /// Who set it.
    pub set_by: ModeSource,
    /// When it was set.
    pub at: DateTime<Utc>,
    /// The invocation that loosened the mode, which itself keeps running under the
    /// stricter mode; the looser mode applies from the next invocation (spec 3.1 §9.2 I14).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_from: Option<String>,
}

impl ModeRecord {
    /// A record of `mode`, set now by `set_by`, at revision 0.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::{HarnessMode, ModeRecord, ModeSource};
    ///
    /// assert_eq!(ModeRecord::new(HarnessMode::Ask, ModeSource::Default).revision, 0);
    /// ```
    pub fn new(mode: HarnessMode, set_by: ModeSource) -> Self {
        Self { mode, revision: 0, set_by, at: Utc::now(), effective_from: None }
    }

    /// Reads a stored record, treating anything unparseable as `ReadOnly` (spec 3.1 §7.4).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::{HarnessMode, ModeRecord};
    /// use serde_json::json;
    ///
    /// assert_eq!(ModeRecord::from_json_lenient(&json!({ "mode": "auto" })).mode, HarnessMode::ReadOnly);
    /// assert_eq!(ModeRecord::from_json_lenient(&json!(42)).mode, HarnessMode::ReadOnly);
    /// ```
    pub fn from_json_lenient(value: &Value) -> Self {
        match serde_json::from_value::<ModeRecord>(value.clone()) {
            Ok(record) => record,
            Err(error) => {
                tracing::warn!(error = %error, "unparseable harness mode record; reading it as read_only");
                let revision = value.get("revision").and_then(Value::as_u64).unwrap_or(0);
                Self {
                    mode: HarnessMode::ReadOnly,
                    revision,
                    set_by: ModeSource::SystemTighten,
                    at: Utc::now(),
                    effective_from: None,
                }
            }
        }
    }

    /// The mode that governs `invocation_id`.
    ///
    /// An `exit_plan_mode` approval loosens the mode from the next invocation, so the
    /// invocation named by [`effective_from`](Self::effective_from) still runs in Plan.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::{HarnessMode, ModeRecord, ModeSource};
    ///
    /// let mut record = ModeRecord::new(HarnessMode::Ask, ModeSource::PlanApproval);
    /// record.effective_from = Some("inv-1".into());
    /// assert_eq!(record.mode_for_invocation("inv-1"), HarnessMode::Plan);
    /// assert_eq!(record.mode_for_invocation("inv-2"), HarnessMode::Ask);
    /// ```
    pub fn mode_for_invocation(&self, invocation_id: &str) -> HarnessMode {
        match &self.effective_from {
            Some(from) if from == invocation_id => {
                let held = if self.set_by == ModeSource::PlanApproval {
                    HarnessMode::Plan
                } else {
                    HarnessMode::ReadOnly
                };
                if self.mode.is_stricter_than(held) { self.mode } else { held }
            }
            _ => self.mode,
        }
    }
}

/// Where a harness keeps each session's mode (spec 3.1 §7.4).
///
/// Keyed by (`app`, `user`, `session`). The policy reads it on every evaluation, so a
/// tightening applies at the next tool call.
///
/// # Example
///
/// ```rust
/// use adk_harness::{HarnessMode, InMemoryModeStore, ModeRecord, ModeSource, ModeStore};
///
/// # futures::executor::block_on(async {
/// let store = InMemoryModeStore::new();
/// store.set("app", "alice", "s1", ModeRecord::new(HarnessMode::Plan, ModeSource::User)).await?;
/// assert_eq!(store.get("app", "alice", "s1").await?.unwrap().mode, HarnessMode::Plan);
/// assert!(store.get("app", "alice", "s2").await?.is_none());
/// # Ok::<(), adk_core::AdkError>(())
/// # }).unwrap();
/// ```
#[async_trait]
pub trait ModeStore: Send + Sync {
    /// The session's record, if one was written.
    async fn get(&self, app: &str, user: &str, session: &str) -> Result<Option<ModeRecord>>;
    /// Replaces the session's record.
    async fn set(&self, app: &str, user: &str, session: &str, record: ModeRecord) -> Result<()>;
}

type ScopeKey = (String, String, String);

fn scope_key(app: &str, user: &str, session: &str) -> ScopeKey {
    (app.to_string(), user.to_string(), session.to_string())
}

/// A [`ModeStore`] in process memory, the builder default (spec 3.1 §7.2).
///
/// # Example
///
/// ```rust
/// let store = adk_harness::InMemoryModeStore::new();
/// # let _ = store;
/// ```
#[derive(Debug, Default)]
pub struct InMemoryModeStore {
    records: RwLock<HashMap<ScopeKey, ModeRecord>>,
}

impl InMemoryModeStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ModeStore for InMemoryModeStore {
    async fn get(&self, app: &str, user: &str, session: &str) -> Result<Option<ModeRecord>> {
        Ok(self.records.read().await.get(&scope_key(app, user, session)).cloned())
    }

    async fn set(&self, app: &str, user: &str, session: &str, record: ModeRecord) -> Result<()> {
        self.records.write().await.insert(scope_key(app, user, session), record);
        Ok(())
    }
}

/// A [`ModeStore`] of one JSON file per session (spec 3.1 §7.4).
///
/// Files are written to a temporary sibling and renamed into place, with mode `0600` on
/// Unix. The default root is `~/.adk/harness/modes`; file names are the SHA-256 of the
/// (app, user, session) triple, so no identifier reaches the path.
///
/// # Example
///
/// ```rust
/// use adk_harness::{FileModeStore, HarnessMode, ModeRecord, ModeSource, ModeStore};
///
/// # tokio_test_block_on(async {
/// let dir = tempfile::tempdir()?;
/// let store = FileModeStore::new(dir.path());
/// store.set("app", "alice", "s1", ModeRecord::new(HarnessMode::Auto, ModeSource::User)).await?;
/// let reopened = FileModeStore::new(dir.path());
/// assert_eq!(reopened.get("app", "alice", "s1").await?.unwrap().mode, HarnessMode::Auto);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # }).unwrap();
/// # fn tokio_test_block_on<F: std::future::Future>(f: F) -> F::Output {
/// #     tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct FileModeStore {
    root: PathBuf,
}

impl FileModeStore {
    /// A store under `root`, created on first write.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// A store under `~/.adk/harness/modes`.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when the home directory cannot be determined.
    pub fn in_home() -> Result<Self> {
        Ok(Self::new(crate::fsutil::adk_home()?.join("harness").join("modes")))
    }

    fn path(&self, app: &str, user: &str, session: &str) -> PathBuf {
        let name = crate::fsutil::sha256_hex(&format!("{app}\u{1f}{user}\u{1f}{session}"));
        self.root.join(format!("{name}.json"))
    }
}

#[async_trait]
impl ModeStore for FileModeStore {
    async fn get(&self, app: &str, user: &str, session: &str) -> Result<Option<ModeRecord>> {
        let path = self.path(app, user, session);
        let Some(bytes) = crate::fsutil::read_optional(&path).await? else {
            return Ok(None);
        };
        let value = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
        Ok(Some(ModeRecord::from_json_lenient(&value)))
    }

    async fn set(&self, app: &str, user: &str, session: &str, record: ModeRecord) -> Result<()> {
        let path = self.path(app, user, session);
        let bytes = serde_json::to_vec_pretty(&record)
            .map_err(|error| crate::fsutil::serialize_error("mode record", &error))?;
        crate::fsutil::write_atomic_private(&path, &bytes).await
    }
}

/// A [`ModeStore`] in SQLite (spec 3.1 §7.1, feature `sqlite`).
///
/// Call [`migrate`](Self::migrate) once before use. The table is
/// `adk_harness_modes`, distinct from every other store's tables.
///
/// # Example
///
/// ```rust,ignore
/// let pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;
/// let store = adk_harness::SqliteModeStore::new(pool);
/// store.migrate().await?;
/// ```
#[cfg(feature = "sqlite")]
#[derive(Debug, Clone)]
pub struct SqliteModeStore {
    pool: sqlx::SqlitePool,
    #[cfg(feature = "encrypted")]
    envelope: Option<crate::encrypted::StoreEnvelope>,
}

#[cfg(feature = "sqlite")]
impl SqliteModeStore {
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
        self.envelope = Some(crate::encrypted::StoreEnvelope::new(keyring, "modes"));
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
        let rows: Vec<(String, String, String, String)> =
            sqlx::query_as("SELECT app_name, user_id, session_id, record FROM adk_harness_modes")
                .fetch_all(&self.pool)
                .await
                .map_err(crate::fsutil::db_error)?;
        let mut changed = 0;
        for (app, user, session, text) in rows {
            if let Some(resealed) = envelope.reseal(&[&app, &user, &session], &text)? {
                sqlx::query("UPDATE adk_harness_modes SET record = ? WHERE app_name = ? AND user_id = ? AND session_id = ?")
                    .bind(resealed)
                    .bind(&app)
                    .bind(&user)
                    .bind(&session)
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
        crate::encrypted::decode_row(self.envelope.as_ref(), "modes", address, text)
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
            "CREATE TABLE IF NOT EXISTS adk_harness_modes (\
                app_name TEXT NOT NULL, user_id TEXT NOT NULL, session_id TEXT NOT NULL, \
                record TEXT NOT NULL, PRIMARY KEY (app_name, user_id, session_id))",
        )
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }
}

#[cfg(feature = "sqlite")]
#[async_trait]
impl ModeStore for SqliteModeStore {
    async fn get(&self, app: &str, user: &str, session: &str) -> Result<Option<ModeRecord>> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT record FROM adk_harness_modes WHERE app_name = ? AND user_id = ? AND session_id = ?",
        )
        .bind(app)
        .bind(user)
        .bind(session)
        .fetch_optional(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        let Some((text,)) = row else {
            return Ok(None);
        };
        let text = self.decode(&[app, user, session], text)?;
        let value = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
        Ok(Some(ModeRecord::from_json_lenient(&value)))
    }

    async fn set(&self, app: &str, user: &str, session: &str, record: ModeRecord) -> Result<()> {
        let text = serde_json::to_string(&record)
            .map_err(|error| crate::fsutil::serialize_error("mode record", &error))?;
        let text = self.encode(&[app, user, session], text)?;
        sqlx::query(
            "INSERT INTO adk_harness_modes (app_name, user_id, session_id, record) VALUES (?, ?, ?, ?) \
             ON CONFLICT (app_name, user_id, session_id) DO UPDATE SET record = excluded.record",
        )
        .bind(app)
        .bind(user)
        .bind(session)
        .bind(text)
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }
}

/// How a requested mode change relates to the current mode (spec 3.1 §7.4).
///
/// # Example
///
/// ```rust
/// use adk_harness::{HarnessMode, ModeChange};
///
/// assert_eq!(ModeChange::between(HarnessMode::Ask, HarnessMode::Plan), ModeChange::Tighten);
/// assert_eq!(ModeChange::between(HarnessMode::Plan, HarnessMode::Ask), ModeChange::Loosen);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeChange {
    /// The new mode is the current one.
    Same,
    /// The new mode is stricter; it applies at the next evaluation.
    Tighten,
    /// The new mode is looser; it needs `Harness::set_mode` or an approved
    /// `exit_plan_mode` (spec 3.1 §7.4).
    Loosen,
}

impl ModeChange {
    /// Classifies a change from `from` to `to`.
    pub fn between(from: HarnessMode, to: HarnessMode) -> Self {
        if from == to {
            Self::Same
        } else if to.is_stricter_than(from) {
            Self::Tighten
        } else {
            Self::Loosen
        }
    }
}

/// Whether `source` may loosen a mode (spec 3.1 §7.4): only a person through
/// `Harness::set_mode` or an approved `exit_plan_mode`.
pub(crate) fn may_loosen(source: ModeSource) -> bool {
    matches!(source, ModeSource::User | ModeSource::PlanApproval)
}

/// Writes a mode change after checking the loosening rule and recording it, write-ahead.
///
/// Returns the stored record. A loosening by any source other than [`ModeSource::User`]
/// or [`ModeSource::PlanApproval`] is refused with `harness.mode.loosening_refused` and
/// recorded as `harness.mode.refused`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn write_mode_change(
    store: &dyn ModeStore,
    default_mode: HarnessMode,
    scope: (&str, &str, &str),
    to: HarnessMode,
    source: ModeSource,
    effective_from: Option<String>,
    audit: Option<&crate::audit::HarnessAudit>,
    cancelled_call_ids: &[String],
) -> Result<ModeRecord> {
    let (app, user, session) = scope;
    let current = store.get(app, user, session).await?;
    let from = current.as_ref().map_or(default_mode, |record| record.mode);
    let change = ModeChange::between(from, to);
    if change == ModeChange::Loosen && !may_loosen(source) {
        if let Some(audit) = audit {
            audit
                .record(
                    "harness.mode.refused",
                    serde_json::json!({
                        "from": from, "to": to, "initiator": source,
                        "reason": "only Harness::set_mode or an approved exit_plan_mode loosens a mode",
                    }),
                )
                .await?;
        }
        return Err(AdkError::new(
            ErrorComponent::Agent,
            ErrorCategory::Forbidden,
            "harness.mode.loosening_refused",
            format!(
                "changing the mode from {from} to {to} loosens it; only Harness::set_mode or an \
                 approved exit_plan_mode may do that"
            ),
        ));
    }
    let record = ModeRecord {
        mode: to,
        revision: current.map_or(1, |record| record.revision + 1),
        set_by: source,
        at: Utc::now(),
        effective_from: effective_from.clone(),
    };
    if let Some(audit) = audit {
        audit
            .record(
                "harness.mode.changed",
                serde_json::json!({
                    "from": from, "to": to, "initiator": source, "revision": record.revision,
                    "effectiveFrom": effective_from, "cancelledHeldCallIds": cancelled_call_ids,
                }),
            )
            .await?;
    }
    store.set(app, user, session, record.clone()).await?;
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_json_reads_as_read_only() {
        for value in [
            serde_json::json!(null),
            serde_json::json!({ "mode": "root" }),
            serde_json::json!({ "mode": "auto", "revision": 3 }),
            serde_json::json!("auto"),
        ] {
            assert_eq!(ModeRecord::from_json_lenient(&value).mode, HarnessMode::ReadOnly);
        }
    }

    #[test]
    fn a_complete_record_round_trips() {
        let record = ModeRecord::new(HarnessMode::Auto, ModeSource::User);
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(ModeRecord::from_json_lenient(&value), record);
    }

    #[tokio::test]
    async fn only_a_person_or_a_plan_approval_loosens_a_mode() {
        let store = InMemoryModeStore::new();
        let scope = ("app", "alice", "s1");
        for source in [ModeSource::Operator, ModeSource::SystemTighten, ModeSource::Default] {
            let refused = write_mode_change(
                &store,
                HarnessMode::Plan,
                scope,
                HarnessMode::Auto,
                source,
                None,
                None,
                &[],
            )
            .await
            .unwrap_err();
            assert_eq!(refused.code, "harness.mode.loosening_refused");
        }
        let tightened = write_mode_change(
            &store,
            HarnessMode::Plan,
            scope,
            HarnessMode::ReadOnly,
            ModeSource::SystemTighten,
            None,
            None,
            &[],
        )
        .await
        .unwrap();
        assert_eq!(tightened.revision, 1);
        for source in [ModeSource::User, ModeSource::PlanApproval] {
            let loosened = write_mode_change(
                &store,
                HarnessMode::Plan,
                scope,
                HarnessMode::Ask,
                source,
                None,
                None,
                &[],
            )
            .await
            .unwrap();
            assert_eq!(loosened.mode, HarnessMode::Ask);
            store
                .set("app", "alice", "s1", ModeRecord::new(HarnessMode::ReadOnly, ModeSource::User))
                .await
                .unwrap();
        }
    }

    #[test]
    fn the_loosening_invocation_keeps_the_stricter_mode() {
        let mut record = ModeRecord::new(HarnessMode::Auto, ModeSource::PlanApproval);
        record.effective_from = Some("inv".into());
        assert_eq!(record.mode_for_invocation("inv"), HarnessMode::Plan);
        record.set_by = ModeSource::User;
        assert_eq!(record.mode_for_invocation("inv"), HarnessMode::ReadOnly);
        assert_eq!(record.mode_for_invocation("other"), HarnessMode::Auto);
    }
}
