//! Crossings (spec 3.2 §7.8, §13 Q3): a request from one partition that reaches another
//! partition's memory, drivers or sessions.
//!
//! | Caller | Decision |
//! |---|---|
//! | Not the coordinator | `Deny`, recorded as a crossing |
//! | The coordinator, into or out of a partition with `cross_partition: deny`, or into a partition its own blocks | `Deny`, recorded |
//! | The coordinator, a read approved earlier in the same run | `Allow`, recorded as a run grant |
//! | The coordinator, otherwise | `RequireApproval`, non-waivable: no standing rule and no `expires_at` rule answers it, and the harness strips run-wide approvals of it |
//!
//! The unit of approval is the [crossing fingerprint](Crossing::fingerprint):
//! `ToolConfirmationRequest::fingerprint()` — the acting agent, the tool's origin, the tool
//! name and the canonical arguments — extended with the target partition and the resource
//! class. An approved read lets the coordinator's identical reads through for the rest of
//! that run and nothing beyond it; writes, and `Financial` and `PublishPublic` driver calls,
//! stay one-shot.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, RwLock};

use adk_core::{
    AdkError, ErrorCategory, ErrorComponent, PolicyCall, PolicyDecision, Tool,
    ToolConfirmationRequest, ToolContext, ToolEffect, ToolOrigin, ToolPolicy, ToolPolicyRequest,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{PartitionId, Partitions};

/// The argument naming the target partition of a crossing tool whose
/// [`CrossingRoute`] is [`Argument`](CrossingRoute::Argument).
pub const CROSSING_PARTITION_ARG: &str = "partition";

/// What a crossing reaches (spec 3.2 §7.8): the resource class of its fingerprint.
///
/// # Example
///
/// ```rust
/// use adk_kernel::CrossingResource;
///
/// assert_eq!(CrossingResource::Driver.as_str(), "driver");
/// assert_eq!(serde_json::to_value(CrossingResource::Memory).unwrap(), "memory");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum CrossingResource {
    /// Another partition's long-term facts or project-scoped memory.
    Memory,
    /// A driver mounted for another partition.
    Driver,
    /// Another partition's sessions.
    Session,
}

impl CrossingResource {
    /// The class's name: `memory`, `driver` or `session`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Driver => "driver",
            Self::Session => "session",
        }
    }
}

impl fmt::Display for CrossingResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a crossing only reads (spec 3.2 §7.8, §13 Q3).
///
/// An approved [`Read`](Self::Read) holds for the coordinator's identical reads for the
/// rest of the run; everything else is [`Write`](Self::Write) and one-shot — a write, and a
/// driver call of effect class `Financial` or `PublishPublic` (K-I9).
///
/// # Example
///
/// ```rust
/// use adk_kernel::CrossingAccess;
///
/// assert_eq!(serde_json::to_value(CrossingAccess::Write).unwrap(), "write");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum CrossingAccess {
    /// Reads only; approved for the rest of the run.
    Read,
    /// Writes or acts; approved one call at a time.
    Write,
}

impl CrossingAccess {
    /// The access's name: `read` or `write`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }
}

/// One request reaching from one partition into another (spec 3.2 §7.8).
///
/// # Example
///
/// ```rust
/// use adk_kernel::{Crossing, CrossingAccess, CrossingResource};
///
/// let crossing = Crossing::new("home", "work", CrossingResource::Driver, "calendar", CrossingAccess::Read);
/// assert_eq!(crossing.label(), "work › calendar");
/// let [allow, deny, block] = crossing.answers();
/// assert_eq!(allow, "allow this read of work › calendar for this run");
/// assert_eq!(deny, "deny");
/// assert_eq!(block, "deny and block crossings to work from this partition");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Crossing {
    /// The partition of the agent making the request.
    pub from: PartitionId,
    /// The partition the request reaches.
    pub to: PartitionId,
    /// What it reaches.
    pub resource: CrossingResource,
    /// The resource's name in the prompt: a driver id, `memory`, a session.
    pub name: String,
    /// Whether it only reads.
    pub access: CrossingAccess,
}

impl Crossing {
    /// A crossing from `from` into `to`'s `resource` named `name`.
    pub fn new(
        from: impl Into<PartitionId>,
        to: impl Into<PartitionId>,
        resource: CrossingResource,
        name: impl Into<String>,
        access: CrossingAccess,
    ) -> Self {
        Self { from: from.into(), to: to.into(), resource, name: name.into(), access }
    }

    /// `<to> › <name>`, as the prompt names the crossing.
    pub fn label(&self) -> String {
        format!("{} › {}", self.to, self.name)
    }

    /// The crossing fingerprint of `request` (spec 3.2 §7.8, §13 Q3): its
    /// [`fingerprint`](ToolConfirmationRequest::fingerprint) — the acting agent, the tool's
    /// origin, the tool name and the canonical arguments — extended with the target
    /// partition and the resource class, separated by U+001F.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::ToolConfirmationRequest;
    /// use adk_kernel::{Crossing, CrossingAccess, CrossingResource};
    /// use serde_json::json;
    ///
    /// let request = ToolConfirmationRequest::new("search", json!({ "q": "dentist" }));
    /// let crossing = |to: &str, resource| Crossing::new("home", to, resource, "memory", CrossingAccess::Read);
    /// let work = crossing("work", CrossingResource::Memory).fingerprint(&request);
    /// assert!(work.starts_with(&request.fingerprint()));
    /// assert_ne!(work, crossing("team", CrossingResource::Memory).fingerprint(&request));
    /// assert_ne!(work, crossing("work", CrossingResource::Session).fingerprint(&request));
    /// ```
    pub fn fingerprint(&self, request: &ToolConfirmationRequest) -> String {
        let mut out = request.fingerprint();
        for part in [self.to.as_str(), self.resource.as_str()] {
            out.push('\u{1f}');
            // A control character would forge a separator.
            out.extend(part.chars().map(|c| if c.is_control() { '\u{fffd}' } else { c }));
        }
        out
    }

    /// The prompt's three answers (spec 3.2 §7.8): allow — for the rest of the run when the
    /// crossing reads, once otherwise — deny, and deny and block crossings to the target
    /// from this partition.
    pub fn answers(&self) -> [String; 3] {
        let allow = match self.access {
            CrossingAccess::Read => format!("allow this read of {} for this run", self.label()),
            CrossingAccess::Write => format!("allow this change to {} once", self.label()),
        };
        [
            allow,
            "deny".to_string(),
            format!("deny and block crossings to {} from this partition", self.to),
        ]
    }

    /// The reason a crossing that needs approval is held with: it names the crossing and
    /// the answers.
    pub fn approval_reason(&self) -> String {
        let [allow, deny, block] = self.answers();
        format!(
            "partition crossing from `{}` into {}: needs a person's approval, which no standing \
             rule can give ({allow} / {deny} / {block})",
            self.from,
            self.label()
        )
    }
}

/// The stored form of a crossing fingerprint: `sha256:` and its hex digest, so the ledger
/// holds no argument text.
pub(crate) fn digest(fingerprint: &str) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;

    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256:");
    for byte in Sha256::digest(fingerprint.as_bytes()) {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// What happened to a crossing (spec 3.2 §9.3 `kernel.partition.crossing`).
///
/// # Example
///
/// ```rust
/// use adk_kernel::CrossingOutcome;
///
/// assert_eq!(serde_json::to_value(CrossingOutcome::RunGrant).unwrap(), "runGrant");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum CrossingOutcome {
    /// Refused at the policy.
    Denied,
    /// Held for a person's approval.
    Held,
    /// Ran after a person approved it; for a read, the approval the rest of the run reuses.
    Approved,
    /// Let through by a read approved earlier in the same run.
    RunGrant,
}

impl CrossingOutcome {
    /// The outcome's name: `denied`, `held`, `approved` or `runGrant`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Denied => "denied",
            Self::Held => "held",
            Self::Approved => "approved",
            Self::RunGrant => "runGrant",
        }
    }
}

/// One entry of the crossing ledger (spec 3.2 §7.8, §9.3): who crossed where, when, in
/// which run, and what happened. It holds a digest of the crossing fingerprint, never the
/// call's arguments.
///
/// # Example
///
/// ```rust
/// use adk_kernel::{CrossingOutcome, CrossingRecord};
///
/// let record: CrossingRecord = serde_json::from_value(serde_json::json!({
///     "id": "c-1", "at": "2026-10-23T09:00:00Z", "run": "run-1", "agent": "urn:adk:agent:app/harness",
///     "from": "home", "to": "work", "resource": "driver", "name": "calendar", "access": "read",
///     "tool": "work__list_events", "fingerprint": "sha256:00", "outcome": "held"
/// })).unwrap();
/// assert_eq!(record.outcome, CrossingOutcome::Held);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct CrossingRecord {
    /// The record's id.
    pub id: String,
    /// When it happened.
    pub at: DateTime<Utc>,
    /// The run: its durable run id, or the invocation id outside a durable run.
    pub run: String,
    /// The acting agent's id.
    pub agent: String,
    /// The partition of the agent.
    pub from: PartitionId,
    /// The partition reached.
    pub to: PartitionId,
    /// What was reached.
    pub resource: CrossingResource,
    /// The resource's name.
    pub name: String,
    /// Whether it only read.
    pub access: CrossingAccess,
    /// The tool called.
    pub tool: String,
    /// `sha256:` digest of the crossing fingerprint.
    pub fingerprint: String,
    /// What happened.
    pub outcome: CrossingOutcome,
    /// Why, for a refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Which crossing records [`PartitionStore::crossings`](super::PartitionStore::crossings)
/// returns. An empty field matches everything.
///
/// # Example
///
/// ```rust
/// use adk_kernel::{CrossingFilter, CrossingOutcome};
///
/// let filter = CrossingFilter::default().from("home").outcomes([CrossingOutcome::Approved]).limit(10);
/// assert_eq!(filter.limit, Some(10));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct CrossingFilter {
    /// Only crossings out of this partition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<PartitionId>,
    /// Only crossings into this partition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<PartitionId>,
    /// Only crossings of this run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Only crossings with this fingerprint digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    /// Only crossings with one of these outcomes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outcomes: Vec<CrossingOutcome>,
    /// Only crossings at or after this time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<DateTime<Utc>>,
    /// At most this many records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

impl CrossingFilter {
    /// Only crossings out of `from`.
    #[must_use]
    pub fn from(mut self, from: impl Into<PartitionId>) -> Self {
        self.from = Some(from.into());
        self
    }

    /// Only crossings into `to`.
    #[must_use]
    pub fn to(mut self, to: impl Into<PartitionId>) -> Self {
        self.to = Some(to.into());
        self
    }

    /// Only crossings of `run`.
    #[must_use]
    pub fn run(mut self, run: impl Into<String>) -> Self {
        self.run = Some(run.into());
        self
    }

    /// Only crossings whose fingerprint digest is `digest`.
    #[must_use]
    pub fn fingerprint(mut self, digest: impl Into<String>) -> Self {
        self.fingerprint = Some(digest.into());
        self
    }

    /// Only crossings with one of `outcomes`.
    #[must_use]
    pub fn outcomes(mut self, outcomes: impl IntoIterator<Item = CrossingOutcome>) -> Self {
        self.outcomes = outcomes.into_iter().collect();
        self
    }

    /// Only crossings at or after `since`.
    #[must_use]
    pub fn since(mut self, since: DateTime<Utc>) -> Self {
        self.since = Some(since);
        self
    }

    /// At most `limit` records.
    #[must_use]
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Whether `record` passes every field but `limit`.
    pub fn matches(&self, record: &CrossingRecord) -> bool {
        self.from.as_ref().is_none_or(|from| *from == record.from)
            && self.to.as_ref().is_none_or(|to| *to == record.to)
            && self.run.as_ref().is_none_or(|run| *run == record.run)
            && self.fingerprint.as_ref().is_none_or(|digest| *digest == record.fingerprint)
            && (self.outcomes.is_empty() || self.outcomes.contains(&record.outcome))
            && self.since.is_none_or(|since| record.at >= since)
    }
}

/// How a crossing tool's call names what it reaches (spec 3.2 §7.8).
///
/// # Example
///
/// ```rust
/// use adk_kernel::{CrossingAccess, CrossingResource, CrossingRoute};
/// use serde_json::json;
///
/// let route = CrossingRoute::argument(CrossingResource::Memory, CrossingAccess::Read, "memory");
/// let crossing = route.crossing(&"home".into(), &json!({ "partition": "work", "query": "x" })).unwrap();
/// assert_eq!(crossing.label(), "work › memory");
/// assert!(route.crossing(&"home".into(), &json!({ "query": "x" })).is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CrossingRoute {
    /// The call's [`partition`](CROSSING_PARTITION_ARG) argument names the target.
    Argument {
        /// What the tool reaches.
        resource: CrossingResource,
        /// Whether it only reads.
        access: CrossingAccess,
        /// The resource's name in the prompt.
        name: String,
    },
    /// The tool reaches one partition: a driver tool of another partition offered to the
    /// coordinator ([`CrossingTool`]).
    Fixed {
        /// The partition it reaches.
        to: PartitionId,
        /// What the tool reaches.
        resource: CrossingResource,
        /// Whether it only reads.
        access: CrossingAccess,
        /// The resource's name in the prompt.
        name: String,
    },
}

impl CrossingRoute {
    /// A route whose call names the target in its `partition` argument.
    pub fn argument(
        resource: CrossingResource,
        access: CrossingAccess,
        name: impl Into<String>,
    ) -> Self {
        Self::Argument { resource, access, name: name.into() }
    }

    /// A route into `to`.
    pub fn fixed(
        to: impl Into<PartitionId>,
        resource: CrossingResource,
        access: CrossingAccess,
        name: impl Into<String>,
    ) -> Self {
        Self::Fixed { to: to.into(), resource, access, name: name.into() }
    }

    /// The crossing a call from `from` with `args` makes.
    ///
    /// # Errors
    ///
    /// The reason, when an [`Argument`](Self::Argument) route's call names no partition.
    pub fn crossing(&self, from: &PartitionId, args: &Value) -> Result<Crossing, String> {
        match self {
            Self::Argument { resource, access, name } => {
                let to = args
                    .get(CROSSING_PARTITION_ARG)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|to| !to.is_empty())
                    .ok_or_else(|| {
                        format!("the call names no `{CROSSING_PARTITION_ARG}` to reach")
                    })?;
                Ok(Crossing::new(from.clone(), to, *resource, name.clone(), *access))
            }
            Self::Fixed { to, resource, access, name } => {
                Ok(Crossing::new(from.clone(), to.clone(), *resource, name.clone(), *access))
            }
        }
    }
}

/// The run an approved read holds for (spec 3.2 §13 Q3): a durable run, across the
/// invocations that resume it, or one invocation outside a durable run.
///
/// A durable run's approved reads are kept in the [`PartitionStore`](super::PartitionStore)
/// and released when the kernel's worker finishes the run; an invocation's are kept in
/// memory and released when its root agent ends. Neither outlives its run, and run and
/// invocation ids are never reused.
///
/// # Example
///
/// ```rust
/// use adk_kernel::CrossingRun;
///
/// assert_eq!(CrossingRun::of(Some("run-1"), "inv-1"), CrossingRun::Durable("run-1".into()));
/// assert_eq!(CrossingRun::of(None, "inv-1").as_str(), "inv-1");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CrossingRun {
    /// A [`DurableRunStore`](adk_core::DurableRunStore) run.
    Durable(String),
    /// An invocation outside a durable run.
    Invocation(String),
}

impl CrossingRun {
    /// The durable run `durable_run_id` when there is one, else the invocation.
    pub fn of(durable_run_id: Option<&str>, invocation_id: &str) -> Self {
        match durable_run_id {
            Some(run) => Self::Durable(run.to_string()),
            None => Self::Invocation(invocation_id.to_string()),
        }
    }

    /// The run's id, as the crossing ledger records it.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Durable(id) | Self::Invocation(id) => id,
        }
    }
}

/// The policy of one partition's harness (spec 3.2 §7.8, K-I15): a `ToolPolicy` that
/// decides the calls of its crossing tools and lets every other call through, so it composes
/// with the harness policy through `StrictestPolicy` and never loosens it (K-I6).
///
/// | Call | Decision |
/// |---|---|
/// | Not a crossing tool, or a crossing into the agent's own partition | `Allow` |
/// | A crossing by any agent but the coordinator | `Deny` |
/// | A crossing the partition table refuses ([`Partitions::admit`]) | `Deny` |
/// | The coordinator's read whose crossing fingerprint was approved earlier in the run | `Allow` |
/// | Any other crossing by the coordinator | `RequireApproval`, naming the crossing and its three answers |
///
/// Every decision about a crossing is recorded in the ledger and as `kernel.partition.crossing`.
/// The run is the call's durable run (`PolicyCall::durable_run_id`), else its invocation.
///
/// # Example
///
/// ```rust
/// use adk_core::{PolicyDecision, ToolPolicy, ToolPolicyRequest};
/// use adk_kernel::{CrossingAccess, CrossingResource, CrossingRoute, Partitions};
/// use serde_json::json;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let policy = Partitions::in_memory()
///     .policy("work")
///     .with_crossing_tool("partition_memory_search", CrossingRoute::argument(CrossingResource::Memory, CrossingAccess::Read, "memory"));
/// let mut request = ToolPolicyRequest::new("partition_memory_search", json!({ "partition": "home", "query": "dentist" }));
/// request.agent_name = "harness".into();
/// // Not the coordinator: denied.
/// assert!(matches!(policy.evaluate(&request).await, PolicyDecision::Deny { .. }));
/// // Any other tool is the harness's to decide.
/// assert_eq!(policy.evaluate(&ToolPolicyRequest::new("read_file", json!({}))).await, PolicyDecision::Allow);
/// # }
/// ```
#[derive(Clone)]
pub struct PartitionPolicy {
    partitions: Partitions,
    home: PartitionId,
    coordinator: Option<String>,
    routes: Arc<RwLock<BTreeMap<String, CrossingRoute>>>,
}

impl fmt::Debug for PartitionPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PartitionPolicy")
            .field("home", &self.home)
            .field("coordinator", &self.coordinator)
            .field("routes", &self.routes.read().unwrap_or_else(|e| e.into_inner()).len())
            .finish()
    }
}

impl PartitionPolicy {
    pub(super) fn new(partitions: Partitions, home: PartitionId) -> Self {
        Self { partitions, home, coordinator: None, routes: Arc::default() }
    }

    /// Makes the agent named `agent_name` — the root agent of this partition's harness —
    /// the coordinator, the one agent that may cross, under approval.
    #[must_use]
    pub fn with_coordinator(mut self, agent_name: impl Into<String>) -> Self {
        self.coordinator = Some(agent_name.into());
        self
    }

    /// Decides calls to the tool `name` as crossings along `route`.
    #[must_use]
    pub fn with_crossing_tool(self, name: impl Into<String>, route: CrossingRoute) -> Self {
        self.register(name, route);
        self
    }

    /// Decides calls to the tool `name` as crossings along `route`, in this policy and every
    /// clone of it.
    pub fn register(&self, name: impl Into<String>, route: CrossingRoute) {
        self.routes.write().unwrap_or_else(|e| e.into_inner()).insert(name.into(), route);
    }

    /// The partition of the harness the policy governs.
    pub fn home(&self) -> &PartitionId {
        &self.home
    }

    /// The coordinator's agent name, when this partition's harness is the coordinator.
    pub fn coordinator(&self) -> Option<&str> {
        self.coordinator.as_deref()
    }

    /// The partition table.
    pub fn partitions(&self) -> &Partitions {
        &self.partitions
    }

    /// The route of the crossing tool `name`, or `None` when the tool is no crossing tool.
    pub fn route(&self, name: &str) -> Option<CrossingRoute> {
        self.routes.read().unwrap_or_else(|e| e.into_inner()).get(name).cloned()
    }

    fn is_coordinator(&self, agent_name: &str) -> bool {
        self.coordinator.as_deref() == Some(agent_name)
    }

    /// Runs `body`, the reach of a crossing tool into another partition, after checking
    /// the crossing the call makes ([`Partitions::admit`]), and records it afterwards: a
    /// crossing tool's `execute` (spec 3.2 §7.8, K-I15).
    ///
    /// The governed path authorized the call before `execute`: the policy held a crossing
    /// for approval unless the run had approved the same read. This check refuses any call
    /// the partition table refuses, so even a harness built without the policy never
    /// crosses for an agent other than the coordinator.
    ///
    /// # Errors
    ///
    /// A `Forbidden` tool error when the crossing is refused, or `body`'s error.
    pub async fn run_crossing<F, Fut>(
        &self,
        ctx: &dyn ToolContext,
        tool: &dyn Tool,
        args: &Value,
        body: F,
    ) -> adk_core::Result<Value>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = adk_core::Result<Value>>,
    {
        let route = self.route(tool.name()).ok_or_else(|| {
            AdkError::tool(format!(
                "{} is not registered as a crossing tool of partition `{}`",
                tool.name(),
                self.home
            ))
        })?;
        let crossing = route.crossing(&self.home, args).map_err(refused)?;
        if crossing.to == self.home {
            return body().await;
        }
        self.partitions
            .admit(&crossing, self.is_coordinator(ctx.agent_name()))
            .await
            .map_err(refused)?;
        let acting = ctx.acting_context();
        let request = ToolConfirmationRequest::new(tool.name(), args.clone())
            .with_acting_context(acting.clone())
            .with_origin(ToolOrigin::of(tool));
        let fingerprint = crossing.fingerprint(&request);
        let durable = ctx.run_config().and_then(|config| config.durable_run_id.clone());
        let run = CrossingRun::of(durable.as_deref(), ctx.invocation_id());
        let value = body().await?;
        if let Err(error) = self
            .partitions
            .executed(&crossing, &fingerprint, &run, acting.agent.id.as_str(), tool.name())
            .await
        {
            tracing::warn!(error = %error, crossing = %crossing.label(), "a crossing ran but could not be recorded");
        }
        Ok(value)
    }
}

fn refused(reason: String) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::Forbidden,
        crate::error::CROSSING_DENIED_CODE,
        reason,
    )
}

#[async_trait]
impl ToolPolicy for PartitionPolicy {
    async fn evaluate(&self, request: &ToolPolicyRequest) -> PolicyDecision {
        self.evaluate_call(request, &PolicyCall::new()).await
    }

    async fn evaluate_call(
        &self,
        request: &ToolPolicyRequest,
        call: &PolicyCall<'_>,
    ) -> PolicyDecision {
        let Some(route) = self.route(&request.tool_name) else {
            return PolicyDecision::Allow;
        };
        let crossing = match route.crossing(&self.home, &request.args) {
            Ok(crossing) => crossing,
            Err(reason) => return PolicyDecision::deny(reason),
        };
        if crossing.to == self.home {
            return PolicyDecision::Allow;
        }
        let mut confirmation =
            ToolConfirmationRequest::new(&request.tool_name, request.args.clone());
        if let Some(acting) = &request.acting {
            confirmation = confirmation.with_acting_context(acting.clone());
        }
        if let Some(origin) = call.origin {
            confirmation = confirmation.with_origin(origin.clone());
        }
        let fingerprint = crossing.fingerprint(&confirmation);
        let run = CrossingRun::of(call.durable_run_id, &request.invocation_id);
        let agent = request
            .acting
            .as_ref()
            .map(|acting| acting.agent.id.as_str().to_string())
            .unwrap_or_else(|| request.agent_name.clone());
        let recorded = super::CrossingCall {
            crossing: &crossing,
            fingerprint: &fingerprint,
            run: run.as_str(),
            agent: &agent,
            tool: &request.tool_name,
        };
        let decided = |outcome, reason: Option<String>| {
            self.partitions.decided(&recorded, call.function_call_id, outcome, reason)
        };
        if let Err(reason) =
            self.partitions.admit(&crossing, self.is_coordinator(&request.agent_name)).await
        {
            tracing::info!(crossing = %crossing.label(), agent.name = %request.agent_name, reason = %reason, "partition crossing denied");
            decided(CrossingOutcome::Denied, Some(reason.clone())).await;
            return PolicyDecision::deny(format!("partition crossing denied: {reason}"));
        }
        if crossing.access == CrossingAccess::Read
            && self.partitions.granted(&run, &fingerprint).await
        {
            decided(CrossingOutcome::RunGrant, None).await;
            return PolicyDecision::Allow;
        }
        decided(CrossingOutcome::Held, None).await;
        PolicyDecision::require_approval(crossing.approval_reason())
    }

    fn policy_ref(&self) -> Option<String> {
        Some(format!("adk-kernel.partition:{}", self.home))
    }
}

/// The most characters of a tool name providers accept.
const TOOL_NAME_MAX: usize = 64;

/// A tool of another partition offered to the coordinator (spec 3.2 §7.8): a driver's tool,
/// or a tool reading another partition's sessions, named `<to>__<tool>` and governed as a
/// crossing by the coordinator's [`PartitionPolicy`], with which it registers itself.
///
/// Its declaration, annotations, origin and effect are the wrapped tool's, under its own
/// name; its description names the crossing. The access is the caller's to state from the
/// tool's effect class — [`Read`](CrossingAccess::Read) only for a driver tool of class
/// `Read`, never for `Financial` or `PublishPublic` — never from the tool's own hints (spec
/// 3.1 I12, K-I9); a `Read` the tool does not also declare `ToolEffect::ReadOnly` for is
/// taken as `Write`, so only a read both sides agree on is reused for the rest of a run.
///
/// # Example
///
/// ```rust,ignore
/// let policy = partitions.policy("home").with_coordinator("harness");
/// let tool = CrossingTool::new(list_events, &policy, "work", CrossingResource::Driver, CrossingAccess::Read, "calendar");
/// assert_eq!(tool.name(), "work__list_events");
/// ```
pub struct CrossingTool {
    inner: Arc<dyn Tool>,
    name: String,
    description: String,
    policy: PartitionPolicy,
}

impl fmt::Debug for CrossingTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CrossingTool").field("name", &self.name).finish_non_exhaustive()
    }
}

impl CrossingTool {
    /// `inner`, a tool reaching partition `to`'s `resource` named `name`, offered to the
    /// coordinator of `policy`'s partition; registers the crossing with `policy`.
    pub fn new(
        inner: Arc<dyn Tool>,
        policy: &PartitionPolicy,
        to: impl Into<PartitionId>,
        resource: CrossingResource,
        access: CrossingAccess,
        name: impl Into<String>,
    ) -> Self {
        let to = to.into();
        // A read is reused for the rest of a run only when the tool also declares itself
        // read-only; a hint never widens what the caller stated (spec 3.1 I12, K-I9).
        let access = match (access, inner.effect()) {
            (CrossingAccess::Read, ToolEffect::ReadOnly) => CrossingAccess::Read,
            _ => CrossingAccess::Write,
        };
        let route = CrossingRoute::fixed(to.clone(), resource, access, name);
        let label = match &route {
            CrossingRoute::Fixed { name, .. } => format!("{to} › {name}"),
            CrossingRoute::Argument { .. } => to.to_string(),
        };
        let tool_name: String = format!("{to}__{}", inner.name())
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') { c } else { '_' })
            .take(TOOL_NAME_MAX)
            .collect();
        let description = format!(
            "[Reaches partition `{to}` ({label}): every call needs the user's approval.] {}",
            inner.description()
        );
        policy.register(tool_name.clone(), route);
        Self { inner, name: tool_name, description, policy: policy.clone() }
    }

    /// The wrapped tool.
    pub fn inner(&self) -> &Arc<dyn Tool> {
        &self.inner
    }
}

#[async_trait]
impl Tool for CrossingTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn declaration(&self) -> Value {
        let mut declaration = self.inner.declaration();
        if let Value::Object(fields) = &mut declaration {
            fields.insert("name".into(), Value::String(self.name.clone()));
            fields.insert("description".into(), Value::String(self.description.clone()));
        }
        declaration
    }

    fn parameters_schema(&self) -> Option<Value> {
        self.inner.parameters_schema()
    }

    fn response_schema(&self) -> Option<Value> {
        self.inner.response_schema()
    }

    fn is_long_running(&self) -> bool {
        self.inner.is_long_running()
    }

    fn annotations(&self) -> Option<&Value> {
        self.inner.annotations()
    }

    fn origin(&self) -> &str {
        self.inner.origin()
    }

    fn required_scopes(&self) -> &[&str] {
        self.inner.required_scopes()
    }

    fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }

    fn effect(&self) -> ToolEffect {
        self.inner.effect()
    }

    fn egress(&self, args: &serde_json::Value) -> adk_core::egress::ToolEgress {
        self.inner.egress(args)
    }

    fn timeout_override(&self) -> Option<Option<std::time::Duration>> {
        self.inner.timeout_override()
    }

    fn is_concurrency_safe(&self) -> bool {
        self.inner.is_concurrency_safe()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let inner = Arc::clone(&self.inner);
        let call_ctx = Arc::clone(&ctx);
        let call_args = args.clone();
        self.policy
            .run_crossing(ctx.as_ref(), self, &args, move || async move {
                inner.execute(call_ctx, call_args).await
            })
            .await
    }
}

/// A crossing that keeps happening (spec 3.2 §7.8): what the maintenance process reports, with
/// partition membership as the remedy, never a waiver.
///
/// # Example
///
/// ```rust
/// use adk_kernel::{CrossingResource, RecurringCrossing};
///
/// let recurring = RecurringCrossing::new("home", "work", CrossingResource::Driver, "calendar", 14);
/// assert!(recurring.remedy().contains("move calendar into home, or merge home and work"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct RecurringCrossing {
    /// The partition the requests came from.
    pub from: PartitionId,
    /// The partition they reached.
    pub to: PartitionId,
    /// What they reached.
    pub resource: CrossingResource,
    /// The resource's name.
    pub name: String,
    /// How many crossings in the window: each request held, denied, or let through by its
    /// run's approval.
    pub count: usize,
}

impl RecurringCrossing {
    /// `count` crossings from `from` into `to`'s `resource` named `name`.
    pub fn new(
        from: impl Into<PartitionId>,
        to: impl Into<PartitionId>,
        resource: CrossingResource,
        name: impl Into<String>,
        count: usize,
    ) -> Self {
        Self { from: from.into(), to: to.into(), resource, name: name.into(), count }
    }

    /// The report line: how often, and that the remedy is partition membership.
    pub fn remedy(&self) -> String {
        format!(
            "partition {} crossed into {} › {} {} times; if it needs it this often, move {} into \
             {}, or merge {} and {} (`adk-rust partitions`); a crossing is never waived",
            self.from, self.to, self.name, self.count, self.name, self.from, self.from, self.to
        )
    }
}
