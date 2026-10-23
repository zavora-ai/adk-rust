//! Driver records, parked driver calls and the [`DriverStore`] (spec 3.2 §7.7, §9.3,
//! §13 Q8, Q9).

use std::collections::BTreeMap;

use adk_core::{ApprovalScope, ToolConfirmationRequest};
use adk_tool::mcp::ParkedMcpCall;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::manifest::DriverManifest;
use crate::error::Result;

/// How a driver's process was isolated when it last started (spec 3.2 §7.7).
///
/// # Example
///
/// ```rust
/// use adk_kernel::drivers::DriverIsolation;
///
/// let isolation = DriverIsolation::Unsandboxed { reason: "no enforcer".into() };
/// assert_eq!(serde_json::to_value(&isolation).unwrap()["kind"], "unsandboxed");
/// assert!(!isolation.is_sandboxed());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum DriverIsolation {
    /// Under the platform's OS sandbox.
    OsSandbox {
        /// The enforcer: `seatbelt`, `bubblewrap`.
        enforcer: String,
    },
    /// On the host with the user's permissions; recorded with why.
    Unsandboxed {
        /// Why the process runs unsandboxed.
        reason: String,
    },
}

impl DriverIsolation {
    /// Whether an enforcer confines the process.
    pub fn is_sandboxed(&self) -> bool {
        matches!(self, Self::OsSandbox { .. })
    }
}

/// A mounted driver: the manifest the user approved, with what the kernel recorded since
/// (spec 3.2 §7.7, §9.3, K-I13).
///
/// # Example
///
/// ```rust
/// use adk_kernel::drivers::{DriverManifest, DriverRecord};
///
/// let manifest = DriverManifest::from_toml("id = \"fs\"\ncommand = \"mcp-fs\"\n").unwrap();
/// let record = DriverRecord::new(manifest, "alice", chrono::Utc::now());
/// assert_eq!(record.id(), "fs");
/// assert!(!record.needs_reapproval());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DriverRecord {
    /// The approved manifest, with each tool's pinned declaration digest.
    pub manifest: DriverManifest,
    /// [`DriverManifest::command_digest`] at approval.
    pub command_digest: String,
    /// Who approved the mount, or the last re-approval.
    pub approved_by: String,
    /// When.
    pub approved_at: DateTime<Utc>,
    /// How the process was isolated when it last started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<DriverIsolation>,
    /// Tools whose declaration differs from the pin, with the digest observed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub stale: BTreeMap<String, String>,
    /// When the record last changed.
    pub updated_at: DateTime<Utc>,
}

impl DriverRecord {
    /// A record of `manifest`, approved by `by` at `at`.
    pub fn new(manifest: DriverManifest, by: impl Into<String>, at: DateTime<Utc>) -> Self {
        Self {
            command_digest: manifest.command_digest(),
            manifest,
            approved_by: by.into(),
            approved_at: at,
            isolation: None,
            stale: BTreeMap::new(),
            updated_at: at,
        }
    }

    /// The driver's id.
    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    /// Whether a tool is stale, so the driver needs re-approval (K-I14).
    pub fn needs_reapproval(&self) -> bool {
        !self.stale.is_empty()
    }
}

/// A driver tool call parked on its server's input request, persisted beside the approval
/// that asks the person: the run's checkpoint payload (spec 3.2 §7.7, §13 Q9).
///
/// The call is keyed by its scope and call id. On wake — a decision, or a restart — the
/// agent dispatches the held call again and the driver re-issues `call` with the answers
/// ([`McpServerManager::reissue_parked_call`](adk_tool::mcp::McpServerManager::reissue_parked_call)).
///
/// # Example
///
/// ```rust
/// use adk_core::{ApprovalScope, ToolConfirmationRequest};
/// use adk_kernel::drivers::DriverPark;
/// use adk_tool::mcp::ParkedMcpCall;
///
/// let call: ParkedMcpCall = serde_json::from_value(serde_json::json!({
///     "serverId": "email", "toolName": "send_draft", "callId": "call-1",
///     "inputRequestsDigest": ParkedMcpCall::digest_input_requests(&Default::default()),
///     "inputRounds": 1, "kind": "mrtr"
/// })).unwrap();
/// let request = ToolConfirmationRequest::new("driver:email/elicitation", serde_json::json!({}));
/// let park = DriverPark::new(ApprovalScope::new("app", "alice", "s1"), "call-1", call, request);
/// assert_eq!(park.driver, "email");
/// assert_eq!(park.fingerprint, park.request.fingerprint());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DriverPark {
    /// The driver the call was made to.
    pub driver: String,
    /// The session the call belongs to.
    pub scope: ApprovalScope,
    /// The agent's function call id.
    pub call_id: String,
    /// The durable run that holds the call, when one does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// The parked MCP call: tool, arguments, `requestState`, input requests and their
    /// digest, and how it resumes.
    pub call: ParkedMcpCall,
    /// The held request that asks the person; its decision answers the input requests.
    pub request: ToolConfirmationRequest,
    /// `request.fingerprint()`, the key of its decision and of the run's wait.
    pub fingerprint: String,
    /// When the call parked.
    pub created_at: DateTime<Utc>,
    /// When the question lapses, if it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// Whether a person approved the call itself before the server asked, so the approval
    /// is carried to the call that resumes, once, until the question lapses.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub carries_approval: bool,
}

impl DriverPark {
    /// The park of `call`, held for `request`, as the agent's call `call_id` in `scope`.
    pub fn new(
        scope: ApprovalScope,
        call_id: impl Into<String>,
        call: ParkedMcpCall,
        request: ToolConfirmationRequest,
    ) -> Self {
        Self {
            driver: call.server_id.clone(),
            scope,
            call_id: call_id.into(),
            run_id: None,
            fingerprint: request.fingerprint(),
            call,
            request,
            created_at: Utc::now(),
            expires_at: None,
            carries_approval: false,
        }
    }

    /// Sets the durable run holding the call.
    #[must_use]
    pub fn with_run_id(mut self, run_id: impl Into<String>) -> Self {
        self.run_id = Some(run_id.into());
        self
    }

    /// Sets when the question lapses.
    #[must_use]
    pub fn with_expires_at(mut self, at: DateTime<Utc>) -> Self {
        self.expires_at = Some(at);
        self
    }
}

/// The kernel's store of mounted drivers and parked driver calls (spec 3.2 §7.7, §13 Q8).
///
/// Runs on every backend that holds the run, schedule and governance stores, with one
/// conformance suite (`driver_store_conformance_tests!`, feature `conformance`):
///
/// | Backend | Type | Feature |
/// |---|---|---|
/// | In-memory | [`InMemoryDriverStore`](crate::stores::InMemoryDriverStore) | `drivers` |
/// | SQLite | `SqliteDriverStore` | `drivers`, `sqlite` |
/// | PostgreSQL | `PostgresDriverStore` | `drivers`, `postgres` |
/// | Redis | `RedisDriverStore` | `drivers`, `redis` |
///
/// # Example
///
/// ```rust
/// use adk_kernel::drivers::{DriverManifest, DriverRecord, DriverStore};
/// use adk_kernel::stores::InMemoryDriverStore;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let store = InMemoryDriverStore::new();
/// let manifest = DriverManifest::from_toml("id = \"fs\"\ncommand = \"mcp-fs\"\n")?;
/// store.put(DriverRecord::new(manifest, "alice", chrono::Utc::now())).await?;
/// assert_eq!(store.get("fs").await?.unwrap().approved_by, "alice");
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait DriverStore: Send + Sync {
    /// Inserts `record`, or replaces the record of the same driver.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn put(&self, record: DriverRecord) -> Result<()>;

    /// The record of driver `id`, or `None`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn get(&self, id: &str) -> Result<Option<DriverRecord>>;

    /// Every record, ordered by driver id.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn list(&self) -> Result<Vec<DriverRecord>>;

    /// Deletes the record of driver `id`; `false` when there was none. Its parked calls
    /// stay until [`delete_park`](Self::delete_park) removes them.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn delete(&self, id: &str) -> Result<bool>;

    /// Inserts `park`, or replaces the park of the same scope and call id.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn put_park(&self, park: DriverPark) -> Result<()>;

    /// The park of call `call_id` in `scope`, or `None`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn get_park(&self, scope: &ApprovalScope, call_id: &str) -> Result<Option<DriverPark>>;

    /// The parks of `driver`, or of every driver, ordered by scope and call id.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn list_parks(&self, driver: Option<&str>) -> Result<Vec<DriverPark>>;

    /// Deletes the park of call `call_id` in `scope`; `false` when there was none.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn delete_park(&self, scope: &ApprovalScope, call_id: &str) -> Result<bool>;
}

/// The order parks list in: scope, then call id.
pub(crate) fn park_key(scope: &ApprovalScope, call_id: &str) -> (String, String, String, String) {
    (scope.app_name.clone(), scope.user_id.clone(), scope.session_id.clone(), call_id.to_string())
}
