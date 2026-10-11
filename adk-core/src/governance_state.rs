//! Durable freeze state for the kill switch, shared by every replica.
//!
//! A [`GovernanceStateStore`] records whether an organisation, a tenant, or one agent is
//! frozen, who froze it, and why. Every change is a compare-and-swap on the scope's
//! `epoch`, so two operators who act at once cannot silently overwrite each other: the
//! second sees `governance.conflict`, re-reads, and decides again. Replicas read the
//! store to learn of a freeze issued anywhere; the store is the durable truth and any
//! notification channel only shortens the time until a replica reads it.
//!
//! | Scope | Key ([`GovernanceScope::key`]) |
//! |-------|-------------------------------|
//! | [`Org`](GovernanceScope::Org) | `org` |
//! | [`Tenant`](GovernanceScope::Tenant) | `tenant:{tenant}` |
//! | [`Agent`](GovernanceScope::Agent) | `agent:{agent id}` |
//!
//! # Backend primitives
//!
//! | Backend | Compare-and-swap on the epoch |
//! |---------|-------------------------------|
//! | SQL | `INSERT ... ON CONFLICT DO NOTHING` for epoch 0, `UPDATE ... WHERE key = $1 AND epoch = $2` otherwise |
//! | Redis | Lua compare of the epoch field |
//! | MongoDB | `insert_one` for epoch 0, `updateOne` filtered on `epoch` otherwise |
//! | Neo4j, Firestore | A transaction that reads the scope record and checks its epoch |

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::principal::{AgentId, SubjectRef};
use crate::{AdkError, Clock, ErrorCategory, ErrorComponent, ErrorDetails, Result, SystemClock};

/// Error code returned when a governance change carries an epoch that moved.
pub const GOVERNANCE_CONFLICT_CODE: &str = "governance.conflict";

/// What a governance state applies to.
///
/// # Example
///
/// ```rust
/// use adk_core::governance_state::GovernanceScope;
///
/// assert_eq!(GovernanceScope::Tenant("acme".into()).key(), "tenant:acme");
/// assert_eq!(serde_json::to_value(GovernanceScope::Org).unwrap(), "org");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum GovernanceScope {
    /// The whole organisation.
    Org,
    /// One tenant.
    Tenant(String),
    /// One agent.
    Agent(AgentId),
}

impl GovernanceScope {
    /// Returns the canonical storage key: `org`, `tenant:{tenant}`, or `agent:{id}`.
    pub fn key(&self) -> String {
        match self {
            Self::Org => "org".to_string(),
            Self::Tenant(tenant) => format!("tenant:{tenant}"),
            Self::Agent(agent) => format!("agent:{agent}"),
        }
    }
}

/// The freeze state of one scope.
///
/// # Example
///
/// ```rust
/// use adk_core::governance_state::{GovernanceScope, GovernanceState};
///
/// let state = GovernanceState {
///     scope: GovernanceScope::Org,
///     frozen: true,
///     reason: Some("incident 42".into()),
///     issued_by: None,
///     issued_at: chrono::Utc::now(),
///     epoch: 1,
/// };
/// assert_eq!(serde_json::to_value(&state).unwrap()["frozen"], true);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceState {
    /// The scope the state applies to.
    pub scope: GovernanceScope,
    /// Whether new runs and further actions in the scope are refused.
    pub frozen: bool,
    /// Why the state was set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Who set the state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued_by: Option<SubjectRef>,
    /// When the state was set.
    pub issued_at: DateTime<Utc>,
    /// Increases by one on every change; 0 means the scope has no state.
    pub epoch: u64,
}

/// Stores freeze state per scope with compare-and-swap updates.
///
/// Every backend passes
/// `adk_core::governance_state_store_conformance_tests!` (feature `conformance`).
#[async_trait]
pub trait GovernanceStateStore: Send + Sync {
    /// Returns the state of `scope`, or `None` when it was never set.
    async fn get(&self, scope: &GovernanceScope) -> Result<Option<GovernanceState>>;

    /// Sets the state of `scope` when its current epoch equals `expected_epoch` (0 when
    /// the scope has no state) and returns the new state at epoch `expected_epoch + 1`.
    ///
    /// # Errors
    ///
    /// Returns [`GOVERNANCE_CONFLICT_CODE`] when the epoch moved; the error's
    /// `current_epoch` metadata holds the epoch to re-read from.
    async fn set(
        &self,
        scope: GovernanceScope,
        expected_epoch: u64,
        frozen: bool,
        reason: Option<String>,
        issued_by: Option<SubjectRef>,
    ) -> Result<GovernanceState>;

    /// Returns every scope's state, ordered by [`GovernanceScope::key`].
    async fn list(&self) -> Result<Vec<GovernanceState>>;
}

/// Builds the error a governance store returns when the epoch moved.
///
/// # Example
///
/// ```rust
/// use adk_core::governance_state::{governance_conflict_error, GovernanceScope, GOVERNANCE_CONFLICT_CODE};
///
/// let error = governance_conflict_error(&GovernanceScope::Org, 1, 2);
/// assert_eq!(error.code, GOVERNANCE_CONFLICT_CODE);
/// assert_eq!(error.details.metadata["current_epoch"], 2);
/// ```
pub fn governance_conflict_error(
    scope: &GovernanceScope,
    expected_epoch: u64,
    current_epoch: u64,
) -> AdkError {
    let mut details = ErrorDetails::default();
    details.metadata.insert("current_epoch".to_string(), current_epoch.into());
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::Conflict,
        GOVERNANCE_CONFLICT_CODE,
        format!(
            "governance state of '{}' is at epoch {current_epoch}, not {expected_epoch}; another \
             change landed first, so re-read the state and decide again",
            scope.key()
        ),
    )
    .with_details(details)
}

/// Process-local [`GovernanceStateStore`].
///
/// State does not survive a restart and is not shared between processes; use a
/// durable backend for a cross-replica kill switch.
///
/// # Example
///
/// ```rust
/// use adk_core::governance_state::{GovernanceScope, GovernanceStateStore, InMemoryGovernanceStateStore};
/// use adk_core::principal::SubjectRef;
///
/// # futures::executor::block_on(async {
/// let store = InMemoryGovernanceStateStore::new();
/// let frozen = store
///     .set(GovernanceScope::Org, 0, true, Some("incident".into()), Some(SubjectRef::human("oncall")))
///     .await?;
/// assert_eq!(frozen.epoch, 1);
/// assert!(store.set(GovernanceScope::Org, 0, false, None, None).await.is_err());
/// # Ok::<(), adk_core::AdkError>(())
/// # }).unwrap();
/// ```
#[derive(Debug)]
pub struct InMemoryGovernanceStateStore {
    states: Mutex<BTreeMap<String, GovernanceState>>,
    clock: Arc<dyn Clock>,
}

impl Default for InMemoryGovernanceStateStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryGovernanceStateStore {
    /// Creates an empty store that reads the system clock.
    pub fn new() -> Self {
        Self { states: Mutex::new(BTreeMap::new()), clock: Arc::new(SystemClock) }
    }

    /// Sets the clock that stamps `issued_at`.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

#[async_trait]
impl GovernanceStateStore for InMemoryGovernanceStateStore {
    async fn get(&self, scope: &GovernanceScope) -> Result<Option<GovernanceState>> {
        Ok(self.states.lock().await.get(&scope.key()).cloned())
    }

    async fn set(
        &self,
        scope: GovernanceScope,
        expected_epoch: u64,
        frozen: bool,
        reason: Option<String>,
        issued_by: Option<SubjectRef>,
    ) -> Result<GovernanceState> {
        let key = scope.key();
        let mut states = self.states.lock().await;
        let current_epoch = states.get(&key).map_or(0, |state| state.epoch);
        if current_epoch != expected_epoch {
            return Err(governance_conflict_error(&scope, expected_epoch, current_epoch));
        }
        let state = GovernanceState {
            scope,
            frozen,
            reason,
            issued_by,
            issued_at: self.clock.now(),
            epoch: current_epoch + 1,
        };
        states.insert(key, state.clone());
        Ok(state)
    }

    async fn list(&self) -> Result<Vec<GovernanceState>> {
        Ok(self.states.lock().await.values().cloned().collect())
    }
}
