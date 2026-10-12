//! The stores a managed session parks on, and how long it waits.
//!
//! Every turn of a managed session runs as a run in a [`DurableRunStore`], one queue per
//! session. A turn that ends waiting on the client — a custom tool result or a tool
//! confirmation — parks its run in `Waiting` until the action's expiry. The client's
//! answer is recorded in an [`ApprovalStore`] and wakes the run; whichever runtime
//! instance hosting the session claims it next resumes the turn, in this process or after
//! a restart. A run whose wait expires is claimed anyway, and each unanswered action is
//! answered with an expiry error.
//!
//! | Store | Holds | Default |
//! |-------|-------|---------|
//! | [`ManagedStateStore`] | Event log, sequence, status, pending actions, agent definition | [`InMemoryManagedStateStore`] |
//! | [`DurableRunStore`] | One run per turn: lease, wait, expiry, wake | [`InMemoryRunStore`] |
//! | [`ApprovalStore`] | Client answers: confirmation decisions and custom tool results | [`InMemoryApprovalStore`] |
//!
//! Every backend of these traits works: the run and approval stores ship for SQLite,
//! PostgreSQL, Redis, MongoDB, Neo4j and Firestore in `adk-session`, and
//! [`SessionServiceStateStore`](crate::state_store::SessionServiceStateStore) keeps managed
//! state in any session backend.
//!
//! # Example
//!
//! ```rust
//! use std::sync::Arc;
//! use std::time::Duration;
//! use adk_core::{InMemoryApprovalStore, InMemoryRunStore};
//! use adk_managed::durable::{DurableConfig, ManagedStores};
//! use adk_managed::InMemoryManagedStateStore;
//!
//! let stores = ManagedStores::new(
//!     Arc::new(InMemoryManagedStateStore::new()),
//!     Arc::new(InMemoryRunStore::new()),
//!     Arc::new(InMemoryApprovalStore::new()),
//! );
//! let config = DurableConfig::default().with_pending_expiry(Some(Duration::from_secs(600)));
//! assert_eq!(config.pending_expiry, Some(Duration::from_secs(600)));
//! # let _ = stores;
//! ```

use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    ApprovalScope, ApprovalService, ApprovalStore, DurableRunStore, InMemoryApprovalStore,
    InMemoryRunStore,
};

use crate::state_store::{InMemoryManagedStateStore, ManagedStateStore};

/// The stores a managed runtime keeps session state, runs, and client answers in.
///
/// Every store is shared by every runtime instance that may host the same sessions: two
/// replicas, or a process and its restart, see the same sessions only through the same
/// stores.
#[derive(Clone)]
pub struct ManagedStores {
    /// Session event log, sequence, status, pending actions, and agent definitions.
    pub state: Arc<dyn ManagedStateStore>,
    /// One run per turn, parked while the session waits on the client.
    pub runs: Arc<dyn DurableRunStore>,
    /// Client answers: confirmation decisions and custom tool results.
    pub decisions: Arc<dyn ApprovalStore>,
    /// The approval inbox held confirmations are submitted to, if any.
    ///
    /// When set, confirmations are decided through the service (quorum, delegation,
    /// escalation, expiry) and `user.tool_confirmation` is refused.
    pub approval_service: Option<Arc<dyn ApprovalService>>,
}

impl std::fmt::Debug for ManagedStores {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedStores")
            .field("state", &self.state)
            .field("decisions", &self.decisions)
            .field("approval_service", &self.approval_service.is_some())
            .finish_non_exhaustive()
    }
}

impl ManagedStores {
    /// Stores over `state`, `runs`, and `decisions`, with no approval service.
    pub fn new(
        state: Arc<dyn ManagedStateStore>,
        runs: Arc<dyn DurableRunStore>,
        decisions: Arc<dyn ApprovalStore>,
    ) -> Self {
        Self { state, runs, decisions, approval_service: None }
    }

    /// In-memory stores: sessions resume within the process, not after it ends.
    pub fn in_memory() -> Self {
        Self::new(
            Arc::new(InMemoryManagedStateStore::new()),
            Arc::new(InMemoryRunStore::new()),
            Arc::new(InMemoryApprovalStore::new()),
        )
    }

    /// Decides held confirmations through `service`.
    ///
    /// The service's own store, when it reports one, replaces
    /// [`decisions`](Self::decisions), because the agent takes the decision from the store
    /// the service writes it to.
    #[must_use]
    pub fn with_approval_service(mut self, service: Arc<dyn ApprovalService>) -> Self {
        if let Some(store) = service.approval_store() {
            self.decisions = store;
        }
        self.approval_service = Some(service);
        self
    }
}

impl Default for ManagedStores {
    fn default() -> Self {
        Self::in_memory()
    }
}

/// How a managed session claims, heartbeats, and parks its runs.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_managed::durable::DurableConfig;
///
/// let config = DurableConfig::default()
///     .with_pending_expiry(None)
///     .with_poll_interval(Duration::from_millis(200))
///     .with_worker_id("replica-a");
/// assert_eq!(config.worker_id, "replica-a");
/// ```
#[derive(Debug, Clone)]
pub struct DurableConfig {
    /// How long a turn waits on the client before each unanswered action expires.
    /// `None` waits until answered.
    pub pending_expiry: Option<Duration>,
    /// How often a hosted session with a parked run checks whether it can resume: woken
    /// by an answer recorded on another runtime instance, or expired.
    pub poll_interval: Duration,
    /// The lease a turn holds on its run; renewed every third of it.
    pub lease_ttl: Duration,
    /// Prefix of the per-session run queue.
    pub queue_prefix: String,
    /// The name this runtime instance claims runs under.
    pub worker_id: String,
}

impl DurableConfig {
    /// The default [`pending_expiry`](Self::pending_expiry): five minutes.
    pub const DEFAULT_PENDING_EXPIRY: Duration = Duration::from_secs(300);

    /// Sets how long a turn waits on the client; `None` waits until answered.
    #[must_use]
    pub fn with_pending_expiry(mut self, expiry: Option<Duration>) -> Self {
        self.pending_expiry = expiry;
        self
    }

    /// Sets how often a parked session checks whether it can resume.
    #[must_use]
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Sets the lease a turn holds on its run.
    #[must_use]
    pub fn with_lease_ttl(mut self, ttl: Duration) -> Self {
        self.lease_ttl = ttl;
        self
    }

    /// Sets the prefix of the per-session run queue.
    #[must_use]
    pub fn with_queue_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.queue_prefix = prefix.into();
        self
    }

    /// Sets the name this runtime instance claims runs under.
    #[must_use]
    pub fn with_worker_id(mut self, worker_id: impl Into<String>) -> Self {
        self.worker_id = worker_id.into();
        self
    }

    /// The run queue of `session_id`.
    pub fn queue(&self, session_id: &str) -> String {
        format!("{}{session_id}", self.queue_prefix)
    }
}

impl Default for DurableConfig {
    fn default() -> Self {
        Self {
            pending_expiry: Some(Self::DEFAULT_PENDING_EXPIRY),
            poll_interval: Duration::from_secs(1),
            lease_ttl: Duration::from_secs(30),
            queue_prefix: "adk-managed:".to_string(),
            worker_id: format!("managed-{}", uuid::Uuid::new_v4()),
        }
    }
}

/// The decision-store scope of a session's client answers.
pub(crate) fn decision_scope(app_name: &str, user_id: &str, session_id: &str) -> ApprovalScope {
    ApprovalScope::new(app_name, user_id, session_id)
}

/// The decision-store key a custom tool result is recorded under.
///
/// Distinct from a confirmation's fingerprint, so a result can never be taken as an
/// approval of a call with the same tool and arguments.
pub(crate) fn custom_result_key(id: &str) -> String {
    format!("custom_tool_result:{id}")
}
