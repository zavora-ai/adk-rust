//! Where managed session state lives, and whether it survives the process.
//!
//! A managed session is a conversation in the `SessionService` plus the runtime's own state:
//! the `SessionEvent` log used for replay, the sequence position, the lifecycle status, the
//! agent definition and owner needed to rebuild the session in another process, and the
//! tool calls waiting on the client. [`ManagedStateStore`] holds that state;
//! [`CheckpointManager`](crate::CheckpointManager) writes it after every event.
//!
//! | Store | [`Durability`] | Backed by |
//! |-------|----------------|-----------|
//! | [`InMemoryManagedStateStore`] | `ProcessLocal` | Process memory (the default) |
//! | [`FileManagedStateStore`] | `CrashDurable` | One fsynced JSON file per session on one host |
//! | [`SessionServiceStateStore`] | As declared for the service | Any `SessionService` backend: SQLite, PostgreSQL, Redis, MongoDB, Neo4j, Firestore, Vertex AI |
//!
//! `SessionServiceStateStore` reuses the session backends rather than adding a bespoke
//! store: every backend already persists session state, so managed state is durable on
//! exactly the backends that hold the conversation, with no new schema to migrate.
//!
//! # Example
//!
//! ```rust
//! use adk_managed::state_store::{Durability, InMemoryManagedStateStore, ManagedStateStore};
//!
//! let store = InMemoryManagedStateStore::new();
//! assert_eq!(store.durability(), Durability::ProcessLocal);
//! assert!(!store.durability().survives_process_loss());
//! ```

use serde::{Deserialize, Serialize};

use crate::checkpoint::RunState;
use crate::types::{ManagedAgentDef, ModelConfig, ModelRef, RuntimeError, SessionEvent};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// What a store guarantees about state after the writing process ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    /// State lives only in this process. Replay and resume work while it runs; a crash loses
    /// everything the store held, and another process cannot resume its sessions.
    ProcessLocal,
    /// State is written to a backing store before the write is acknowledged, so another
    /// process can reconstruct a session after loss.
    CrashDurable,
}

impl Durability {
    /// Whether state written to this store outlives the process that wrote it.
    ///
    /// Callers that require durability should check this at startup rather than infer it from
    /// the presence of checkpointing.
    pub fn survives_process_loss(&self) -> bool {
        matches!(self, Durability::CrashDurable)
    }
}

/// A snapshot of one managed session's state.
///
/// Not `PartialEq`: `SessionEvent` is `#[non_exhaustive]` and not comparable, so compare the
/// `run_state` and event count rather than whole snapshots.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedSessionState {
    /// Events recorded for the session, in order.
    pub events: Vec<SessionEvent>,
    /// Sequence position, parked tool calls, and lifecycle status.
    pub run_state: RunState,
    /// The owner and agent definition another process rebuilds the session from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<SessionRecord>,
    /// Tool calls waiting on the client: custom tool results and confirmations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<PendingAction>,
    /// The durable run parked on [`pending`](Self::pending), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_run_id: Option<String>,
}

impl ManagedSessionState {
    /// A snapshot with `events` and `run_state` and nothing else.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_managed::{ManagedSessionState, RunState};
    ///
    /// let state = ManagedSessionState::new(Vec::new(), RunState::initial());
    /// assert!(state.pending.is_empty() && state.record.is_none());
    /// ```
    pub fn new(events: Vec<SessionEvent>, run_state: RunState) -> Self {
        Self { events, run_state, record: None, pending: Vec::new(), durable_run_id: None }
    }
}

/// Who a session belongs to and the agent it runs, as stored for rebuilding the session.
///
/// The API key of an OpenAI-compatible model reference is not stored; the
/// [`ModelResolver`](crate::ModelResolver) supplies it again when the session is rebuilt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    /// The app the session belongs to.
    pub app_name: String,
    /// The user the session belongs to.
    pub user_id: String,
    /// The agent definition, with secrets removed.
    pub agent: ManagedAgentDef,
}

impl SessionRecord {
    /// Records `agent` for `app_name` / `user_id`, removing the API key of an
    /// OpenAI-compatible model reference.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_managed::state_store::SessionRecord;
    /// use adk_managed::types::{ManagedAgentDef, ModelConfig, ModelRef, Provider};
    ///
    /// let agent = ManagedAgentDef::new("a", ModelRef::Structured {
    ///     provider: Provider::OpenaiCompatible,
    ///     model: ModelConfig::Compatible {
    ///         model: "m".into(),
    ///         base_url: "https://llm.example/v1".into(),
    ///         api_key: "sk-secret".into(),
    ///     },
    ///     speed: None,
    /// });
    /// let record = SessionRecord::new("app", "user", agent);
    /// assert!(!serde_json::to_string(&record).unwrap().contains("sk-secret"));
    /// ```
    pub fn new(
        app_name: impl Into<String>,
        user_id: impl Into<String>,
        agent: ManagedAgentDef,
    ) -> Self {
        let mut agent = agent;
        if let ModelRef::Structured { model: ModelConfig::Compatible { api_key, .. }, .. } =
            &mut agent.model
        {
            api_key.clear();
        }
        Self { app_name: app_name.into(), user_id: user_id.into(), agent }
    }
}

/// What a [`PendingAction`] waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingKind {
    /// The client executes a custom tool and sends `user.custom_tool_result`.
    CustomTool,
    /// The client allows or denies a tool call with `user.tool_confirmation`.
    Confirmation,
}

/// A tool call the session waits on the client for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingAction {
    /// The ID the client answers with: `custom_tool_use_id` or `tool_use_id`.
    pub id: String,
    /// What the action waits for.
    pub kind: PendingKind,
    /// The tool called.
    pub tool_name: String,
    /// The model's call ID, when the provider assigned one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_call_id: Option<String>,
    /// The key the client's answer is recorded under in the decision store.
    pub decision_key: String,
    /// The call's arguments.
    #[serde(default)]
    pub input: serde_json::Value,
    /// When the action expires; `None` waits until answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// A custom tool result already taken from the decision store, held until every
    /// action of the turn is answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<serde_json::Value>,
}

/// Storage for managed session state.
///
/// Implementations decide the durability guarantee and must report it truthfully through
/// [`ManagedStateStore::durability`], because that is what a caller uses to decide whether
/// resume-after-restart is available.
#[async_trait]
pub trait ManagedStateStore: Send + Sync + std::fmt::Debug {
    /// What this store guarantees after process loss.
    fn durability(&self) -> Durability;

    /// Records the state of `session_id`, replacing any previous snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when the backing store rejects the write. A durable
    /// implementation must not acknowledge before the write is persisted, since the whole
    /// point of the guarantee is that an acknowledged checkpoint is recoverable.
    async fn save(&self, session_id: &str, state: ManagedSessionState) -> Result<(), RuntimeError>;

    /// The recorded state for `session_id`, or `None` when nothing is stored.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when the backing store cannot be read.
    async fn load(&self, session_id: &str) -> Result<Option<ManagedSessionState>, RuntimeError>;

    /// Removes any state for `session_id`.
    ///
    /// Succeeds when nothing was stored, so deletion is idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when the backing store cannot complete the removal.
    async fn delete(&self, session_id: &str) -> Result<(), RuntimeError>;

    /// The sessions this store holds state for.
    ///
    /// A durable implementation uses this at startup to reconstruct sessions; a process-local
    /// one only ever reports sessions from the current process.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when the backing store cannot be enumerated.
    async fn session_ids(&self) -> Result<Vec<String>, RuntimeError>;
}

/// The in-memory managed state store.
///
/// The default. Named explicitly so its guarantee is visible at the call site.
#[derive(Debug, Default)]
pub struct InMemoryManagedStateStore {
    sessions: Arc<RwLock<HashMap<String, ManagedSessionState>>>,
}

impl InMemoryManagedStateStore {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ManagedStateStore for InMemoryManagedStateStore {
    fn durability(&self) -> Durability {
        Durability::ProcessLocal
    }

    async fn save(&self, session_id: &str, state: ManagedSessionState) -> Result<(), RuntimeError> {
        self.sessions.write().await.insert(session_id.to_string(), state);
        Ok(())
    }

    async fn load(&self, session_id: &str) -> Result<Option<ManagedSessionState>, RuntimeError> {
        Ok(self.sessions.read().await.get(session_id).cloned())
    }

    async fn delete(&self, session_id: &str) -> Result<(), RuntimeError> {
        self.sessions.write().await.remove(session_id);
        Ok(())
    }

    async fn session_ids(&self) -> Result<Vec<String>, RuntimeError> {
        Ok(self.sessions.read().await.keys().cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::RunState;
    use crate::types::SessionStatus;

    fn state() -> ManagedSessionState {
        ManagedSessionState::new(
            Vec::new(),
            RunState {
                seq: 7,
                pending_tool_ids: vec!["call-1".to_string()],
                status: SessionStatus::Running,
            },
        )
    }

    #[tokio::test]
    async fn the_in_memory_store_reports_its_own_guarantee() {
        let store = InMemoryManagedStateStore::new();
        assert_eq!(store.durability(), Durability::ProcessLocal);
        assert!(
            !store.durability().survives_process_loss(),
            "a caller requiring resume-after-restart must be able to detect that it is absent"
        );
    }

    #[tokio::test]
    async fn a_saved_snapshot_round_trips() {
        let store = InMemoryManagedStateStore::new();
        store.save("session-1", state()).await.unwrap();

        let loaded = store.load("session-1").await.unwrap().expect("saved state must load");
        assert_eq!(loaded.run_state, state().run_state);
        assert_eq!(loaded.events.len(), state().events.len());
        assert_eq!(store.session_ids().await.unwrap(), vec!["session-1".to_string()]);
    }

    #[tokio::test]
    async fn an_unknown_session_loads_as_none_and_deletes_without_error() {
        let store = InMemoryManagedStateStore::new();
        assert!(store.load("missing").await.unwrap().is_none());
        assert!(store.delete("missing").await.is_ok(), "deletion is idempotent");
    }

    #[tokio::test]
    async fn saving_twice_replaces_the_snapshot() {
        let store = InMemoryManagedStateStore::new();
        store.save("session-1", state()).await.unwrap();

        let mut later = state();
        later.run_state.seq = 9;
        store.save("session-1", later.clone()).await.unwrap();

        let loaded = store.load("session-1").await.unwrap().expect("state must load");
        assert_eq!(loaded.run_state.seq, later.run_state.seq);
        assert_eq!(store.session_ids().await.unwrap().len(), 1, "not appended twice");
    }

    #[tokio::test]
    async fn a_new_store_shares_nothing_with_the_old_one() {
        // This is the shape of the gap: state written by one store is invisible to another,
        // which is what happens across a process restart. A `CrashDurable` implementation
        // backed by shared storage would find the session here.
        let first = InMemoryManagedStateStore::new();
        first.save("session-1", state()).await.unwrap();

        let second = InMemoryManagedStateStore::new();
        assert!(
            second.load("session-1").await.unwrap().is_none(),
            "process-local state does not cross process boundaries"
        );
        assert!(second.session_ids().await.unwrap().is_empty());
    }
}

/// Records each session as one JSON file under a directory.
///
/// Reports [`Durability::CrashDurable`]: a write is fsynced through a temporary file and a
/// rename, so another process on the same host can reconstruct a session after loss.
///
/// One file per session rather than one file for all of them, so two sessions being
/// checkpointed at once do not contend, and a corrupt write can only lose the session
/// it belonged to.
///
/// # Example
///
/// ```no_run
/// use adk_managed::state_store::{Durability, FileManagedStateStore, ManagedStateStore};
///
/// let store = FileManagedStateStore::new("/var/lib/adk/sessions");
/// assert_eq!(store.durability(), Durability::CrashDurable);
/// ```
#[derive(Debug)]
pub struct FileManagedStateStore {
    root: std::path::PathBuf,
}

impl FileManagedStateStore {
    /// Records sessions under `root`, creating it on the first write.
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The file holding one session.
    ///
    /// The id is percent-style escaped, so an id containing a path separator cannot
    /// write outside `root`.
    fn path_for(&self, session_id: &str) -> std::path::PathBuf {
        let safe: String = session_id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        self.root.join(format!("{safe}.json"))
    }

    fn failed(action: &str, error: impl std::fmt::Display) -> RuntimeError {
        RuntimeError::CheckpointFailed {
            message: format!("could not {action} session state: {error}"),
        }
    }
}

#[async_trait::async_trait]
impl ManagedStateStore for FileManagedStateStore {
    fn durability(&self) -> Durability {
        Durability::CrashDurable
    }

    async fn save(&self, session_id: &str, state: ManagedSessionState) -> Result<(), RuntimeError> {
        // `std::fs` rather than `tokio::fs`: this crate does not enable tokio's `fs`
        // feature, and relying on another crate in the workspace to enable it would
        // break the moment this crate is built alone. A snapshot is small.
        use std::io::Write;

        let path = self.path_for(session_id);
        let text = serde_json::to_vec_pretty(&state).map_err(|e| Self::failed("encode", e))?;
        std::fs::create_dir_all(&self.root)
            .map_err(|e| Self::failed("create the directory for", e))?;

        // Written beside the target, synced, then renamed. The sync is what lets this
        // store claim CrashDurable: the trait forbids acknowledging a write that is not
        // yet persisted, and the rename means a reader sees a whole snapshot or none.
        let temporary = path.with_extension("json.tmp");
        let mut file = std::fs::File::create(&temporary)
            .map_err(|e| Self::failed("open a temporary file for", e))?;
        file.write_all(&text).map_err(|e| Self::failed("write", e))?;
        file.sync_all().map_err(|e| Self::failed("sync", e))?;
        drop(file);
        std::fs::rename(&temporary, &path).map_err(|e| Self::failed("commit", e))
    }

    async fn load(&self, session_id: &str) -> Result<Option<ManagedSessionState>, RuntimeError> {
        match std::fs::read(self.path_for(session_id)) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map(Some).map_err(|e| Self::failed("decode", e))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Self::failed("read", error)),
        }
    }

    async fn delete(&self, session_id: &str) -> Result<(), RuntimeError> {
        match std::fs::remove_file(self.path_for(session_id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(Self::failed("delete", error)),
        }
    }

    async fn session_ids(&self) -> Result<Vec<String>, RuntimeError> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(Self::failed("list", error)),
        };
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| Self::failed("list", e))?;
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(id) = name.strip_suffix(".json") {
                ids.push(id.to_string());
            }
        }
        ids.sort();
        Ok(ids)
    }
}

/// Records managed session state in a [`SessionService`](adk_session::SessionService).
///
/// Each managed session gets a companion session in a namespace of its own (app
/// `adk-managed`, user `managed-state` by default) whose state holds the snapshot. A save
/// appends one event carrying a state delta: the snapshot's control fields, and only the
/// session events added since the previous save, so a session's stored size grows with its
/// event count rather than with the square of it.
///
/// Durability is the backend's: SQLite, PostgreSQL, Redis, MongoDB, Neo4j, Firestore and
/// Vertex AI sessions survive the process, `InMemorySessionService` does not. A
/// `dyn SessionService` cannot report which it is, so the constructor takes the guarantee
/// of the service passed to it.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_managed::state_store::{Durability, ManagedStateStore, SessionServiceStateStore};
/// use adk_session::InMemorySessionService;
///
/// let store = SessionServiceStateStore::new(
///     Arc::new(InMemorySessionService::new()),
///     Durability::ProcessLocal,
/// );
/// assert_eq!(store.durability(), Durability::ProcessLocal);
/// ```
pub struct SessionServiceStateStore {
    service: Arc<dyn adk_session::SessionService>,
    durability: Durability,
    app_name: String,
    user_id: String,
    /// Events already written per session, so a save writes only the new ones.
    written: tokio::sync::Mutex<HashMap<String, usize>>,
}

impl std::fmt::Debug for SessionServiceStateStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionServiceStateStore")
            .field("durability", &self.durability)
            .field("app_name", &self.app_name)
            .field("user_id", &self.user_id)
            .finish_non_exhaustive()
    }
}

/// The state key holding everything in a snapshot except its events.
const SNAPSHOT_KEY: &str = "managed_snapshot";
/// The prefix of the state keys holding one session event each, by position.
const EVENT_KEY_PREFIX: &str = "managed_event_";

impl SessionServiceStateStore {
    /// The app name companion sessions are created under by default.
    pub const DEFAULT_APP_NAME: &'static str = "adk-managed";
    /// The user id companion sessions are created under by default.
    pub const DEFAULT_USER_ID: &'static str = "managed-state";

    /// Records state in `service`, whose guarantee after process loss is `durability`.
    pub fn new(service: Arc<dyn adk_session::SessionService>, durability: Durability) -> Self {
        Self {
            service,
            durability,
            app_name: Self::DEFAULT_APP_NAME.to_string(),
            user_id: Self::DEFAULT_USER_ID.to_string(),
            written: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Creates companion sessions under `app_name` / `user_id` instead of the defaults.
    #[must_use]
    pub fn with_namespace(
        mut self,
        app_name: impl Into<String>,
        user_id: impl Into<String>,
    ) -> Self {
        self.app_name = app_name.into();
        self.user_id = user_id.into();
        self
    }

    fn failed(action: &str, error: impl std::fmt::Display) -> RuntimeError {
        RuntimeError::CheckpointFailed {
            message: format!(
                "could not {action} managed session state in the session service: {error}"
            ),
        }
    }

    fn identity(&self, session_id: &str) -> Result<adk_core::AdkIdentity, RuntimeError> {
        Ok(adk_core::AdkIdentity::new(
            adk_core::AppName::try_from(self.app_name.as_str())
                .map_err(|e| Self::failed("address", e))?,
            adk_core::UserId::try_from(self.user_id.as_str())
                .map_err(|e| Self::failed("address", e))?,
            adk_core::SessionId::try_from(session_id).map_err(|e| Self::failed("address", e))?,
        ))
    }

    /// The stored state map of `session_id`, or `None` when no companion session exists.
    async fn stored(
        &self,
        session_id: &str,
    ) -> Result<Option<HashMap<String, serde_json::Value>>, RuntimeError> {
        // `get` is the call whose contract reports a missing session as `session.not_found`.
        let request = adk_session::GetRequest {
            app_name: self.app_name.clone(),
            user_id: self.user_id.clone(),
            session_id: session_id.to_string(),
            num_recent_events: None,
            after: None,
        };
        match self.service.get(request).await {
            Ok(session) => Ok(Some(session.state().all())),
            Err(error) if error.is_not_found() => Ok(None),
            Err(error) => Err(Self::failed("read", error)),
        }
    }
}

#[async_trait]
impl ManagedStateStore for SessionServiceStateStore {
    fn durability(&self) -> Durability {
        self.durability
    }

    async fn save(&self, session_id: &str, state: ManagedSessionState) -> Result<(), RuntimeError> {
        let mut written = self.written.lock().await;
        let already = match written.get(session_id) {
            Some(count) => *count,
            None => match self.stored(session_id).await? {
                Some(stored) => {
                    stored.keys().filter(|key| key.starts_with(EVENT_KEY_PREFIX)).count()
                }
                None => {
                    self.service
                        .create(adk_session::CreateRequest {
                            app_name: self.app_name.clone(),
                            user_id: self.user_id.clone(),
                            session_id: Some(session_id.to_string()),
                            state: HashMap::new(),
                        })
                        .await
                        .map_err(|e| Self::failed("create", e))?;
                    0
                }
            },
        };

        let ManagedSessionState { events, run_state, record, pending, durable_run_id } = state;
        let total = events.len();
        let mut delta = HashMap::new();
        for (index, event) in events.into_iter().enumerate().skip(already) {
            let value = serde_json::to_value(event).map_err(|e| Self::failed("encode", e))?;
            delta.insert(format!("{EVENT_KEY_PREFIX}{index:012}"), value);
        }
        let snapshot =
            ManagedSessionState { events: Vec::new(), run_state, record, pending, durable_run_id };
        delta.insert(
            SNAPSHOT_KEY.to_string(),
            serde_json::to_value(snapshot).map_err(|e| Self::failed("encode", e))?,
        );

        let mut event = adk_core::Event::new(format!("managed-state-{}", uuid::Uuid::new_v4()));
        event.author = "adk-managed".to_string();
        event.actions.state_delta = delta;
        self.service
            .append_event_for_identity(adk_session::AppendEventRequest {
                identity: self.identity(session_id)?,
                event,
            })
            .await
            .map_err(|e| Self::failed("write", e))?;
        written.insert(session_id.to_string(), total.max(already));
        Ok(())
    }

    async fn load(&self, session_id: &str) -> Result<Option<ManagedSessionState>, RuntimeError> {
        let Some(stored) = self.stored(session_id).await? else {
            return Ok(None);
        };
        let Some(snapshot) = stored.get(SNAPSHOT_KEY) else {
            return Ok(None);
        };
        let mut state: ManagedSessionState =
            serde_json::from_value(snapshot.clone()).map_err(|e| Self::failed("decode", e))?;
        let mut events: Vec<(&String, &serde_json::Value)> =
            stored.iter().filter(|(key, _)| key.starts_with(EVENT_KEY_PREFIX)).collect();
        events.sort_by(|a, b| a.0.cmp(b.0));
        state.events = events
            .into_iter()
            .map(|(_, value)| serde_json::from_value(value.clone()))
            .collect::<Result<_, _>>()
            .map_err(|e| Self::failed("decode", e))?;
        self.written.lock().await.insert(session_id.to_string(), state.events.len());
        Ok(Some(state))
    }

    async fn delete(&self, session_id: &str) -> Result<(), RuntimeError> {
        self.written.lock().await.remove(session_id);
        let request = adk_session::DeleteRequest {
            app_name: self.app_name.clone(),
            user_id: self.user_id.clone(),
            session_id: session_id.to_string(),
        };
        match self.service.delete(request).await {
            Ok(()) => Ok(()),
            Err(error) if error.is_not_found() => Ok(()),
            Err(error) => Err(Self::failed("delete", error)),
        }
    }

    async fn session_ids(&self) -> Result<Vec<String>, RuntimeError> {
        let sessions = self
            .service
            .list(adk_session::ListRequest {
                app_name: self.app_name.clone(),
                user_id: self.user_id.clone(),
                limit: None,
                offset: None,
            })
            .await
            .map_err(|e| Self::failed("list", e))?;
        let mut ids: Vec<String> =
            sessions.iter().map(|session| session.id().to_string()).collect();
        ids.sort();
        Ok(ids)
    }
}
