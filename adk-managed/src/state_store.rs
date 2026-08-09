//! Where managed session state lives, and whether it survives the process.
//!
//! [`CheckpointManager`](crate::CheckpointManager) holds events and run state in a `Vec` and a
//! struct field. That supports replay and resume *within* a process and nothing more: a crash
//! loses event history, parked-tool state, sequence position, and lifecycle status even when the
//! nested Runner has already written conversation events through the `SessionService`. A new
//! process cannot resume a session another process started.
//!
//! The problem was not the in-memory implementation — it is a reasonable default — but that
//! nothing distinguished it from a durable one. There was no seam to implement against, no way
//! for a caller to ask what guarantee it had, and the crate described itself as durable.
//!
//! [`ManagedStateStore`] is that seam. [`InMemoryManagedStateStore`] is the in-memory backend,
//! named as such, reporting [`Durability::ProcessLocal`]. A durable implementation is not
//! shipped; a caller can now detect that instead of assuming otherwise.
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
use crate::types::{RuntimeError, SessionEvent};
use async_trait::async_trait;
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
/// The default, and the only implementation that ships. Named explicitly so its guarantee is
/// visible at the call site rather than implied by the absence of an alternative.
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
        ManagedSessionState {
            events: Vec::new(),
            run_state: RunState {
                seq: 7,
                pending_tool_ids: vec!["call-1".to_string()],
                status: SessionStatus::Running,
            },
        }
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
/// The first store here that reports [`Durability::CrashDurable`]: a write is fsynced
/// through a temporary file and a rename, so another process can reconstruct a session
/// after loss. `InMemoryManagedStateStore` cannot, and says so.
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
