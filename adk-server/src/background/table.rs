//! A live table of agent targets that [`DurableWorker`](super::DurableWorker)s read on
//! every claim (spec 3.3 §10 S4).
//!
//! A worker built with [`with_agent`](super::DurableWorker::with_agent) knows its targets
//! from the start. A host that adds agents while its workers run — the kernel's agents a
//! person adds (spec 3.3 §5.7) — shares one [`AgentTable`] with every worker through
//! [`with_agent_table`](super::DurableWorker::with_agent_table) and changes it in place: a
//! target [`insert`](AgentTable::insert)ed serves the next claim on every queue, and a
//! target [`remove`](AgentTable::remove)d is refused from the next claim on.
//!
//! A replica that has not built a target yet still knows it should: the host marks it
//! [`expect`](AgentTable::expect)ed, and a worker hands a claimed run of an expected target
//! that is not built back to its queue for a later claim instead of failing it.
//!
//! | Target | A claim of its run |
//! |---|---|
//! | Built (in the table) | Runs on the table's runner |
//! | Expected, not built | Handed back, claimable again one poll interval later ([`ParkReason::HandedBack`](super::ParkReason::HandedBack)) |
//! | Neither | Fails without retry, as an unknown target always has |

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

use adk_runner::Runner;

/// A shared, cloneable map of agent target → [`Runner`], with the set of targets the host
/// expects to serve (spec 3.3 §10 S4). Clones share one table.
///
/// # Example
///
/// ```rust
/// use adk_server::background::AgentTable;
///
/// let table = AgentTable::new();
/// table.expect("finance");
/// assert!(table.is_expected("finance"));
/// assert!(table.get("finance").is_none(), "expected, not built yet");
/// assert!(table.forget("finance"));
/// assert!(!table.is_expected("finance"));
/// ```
#[derive(Clone, Default)]
pub struct AgentTable {
    // A std lock: workers read it from the synchronous `has_target`, and no guard is held
    // across an await.
    inner: Arc<RwLock<Entries>>,
}

#[derive(Default)]
struct Entries {
    agents: BTreeMap<String, Arc<Runner>>,
    expected: BTreeSet<String>,
}

impl std::fmt::Debug for AgentTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let entries = self.read();
        f.debug_struct("AgentTable")
            .field("agents", &entries.agents.keys().collect::<Vec<_>>())
            .field("expected", &entries.expected)
            .finish()
    }
}

impl AgentTable {
    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Entries> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Entries> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Hosts `runner` as `target`, replacing the runner it had; returns the replaced one.
    ///
    /// A claim already running on the replaced runner finishes on it; the next claim of
    /// `target` runs on `runner`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let table = AgentTable::new();
    /// assert!(table.insert("finance", Arc::new(runner)).is_none());
    /// assert!(table.contains("finance"));
    /// ```
    pub fn insert(&self, target: impl Into<String>, runner: Arc<Runner>) -> Option<Arc<Runner>> {
        self.write().agents.insert(target.into(), runner)
    }

    /// Stops hosting `target`; returns its runner. The target stays expected until
    /// [`forget`](Self::forget), so a claim of it is handed back rather than failed.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_server::background::AgentTable;
    ///
    /// assert!(AgentTable::new().remove("finance").is_none());
    /// ```
    pub fn remove(&self, target: &str) -> Option<Arc<Runner>> {
        self.write().agents.remove(target)
    }

    /// The runner hosted as `target`, if any.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_server::background::AgentTable;
    ///
    /// assert!(AgentTable::new().get("finance").is_none());
    /// ```
    pub fn get(&self, target: &str) -> Option<Arc<Runner>> {
        self.read().agents.get(target).cloned()
    }

    /// Whether a runner is hosted as `target`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_server::background::AgentTable;
    ///
    /// assert!(!AgentTable::new().contains("finance"));
    /// ```
    pub fn contains(&self, target: &str) -> bool {
        self.read().agents.contains_key(target)
    }

    /// The hosted targets, sorted.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_server::background::AgentTable;
    ///
    /// assert!(AgentTable::new().targets().is_empty());
    /// ```
    pub fn targets(&self) -> Vec<String> {
        self.read().agents.keys().cloned().collect()
    }

    /// Marks `target` as one the host serves, built or not: a claim of it before it is
    /// built is handed back for a later claim.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_server::background::AgentTable;
    ///
    /// let table = AgentTable::new();
    /// table.expect("finance");
    /// assert_eq!(table.expected(), ["finance"]);
    /// ```
    pub fn expect(&self, target: impl Into<String>) {
        self.write().expected.insert(target.into());
    }

    /// Stops expecting `target`; returns whether it was expected. A claim of it that no
    /// runner serves then fails as an unknown target's does.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_server::background::AgentTable;
    ///
    /// assert!(!AgentTable::new().forget("finance"));
    /// ```
    pub fn forget(&self, target: &str) -> bool {
        self.write().expected.remove(target)
    }

    /// Whether the host expects to serve `target`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_server::background::AgentTable;
    ///
    /// assert!(!AgentTable::new().is_expected("finance"));
    /// ```
    pub fn is_expected(&self, target: &str) -> bool {
        self.read().expected.contains(target)
    }

    /// The expected targets, sorted.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_server::background::AgentTable;
    ///
    /// assert!(AgentTable::new().expected().is_empty());
    /// ```
    pub fn expected(&self) -> Vec<String> {
        self.read().expected.iter().cloned().collect()
    }
}
