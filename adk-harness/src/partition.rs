//! Partitions in the harness (spec 3.2 §7.8, §13 Q3, K-I15; feature `kernel`).
//!
//! One `HarnessAgent` per partition, built with
//! [`HarnessAgentBuilder::partition`](crate::HarnessAgentBuilder::partition):
//!
//! | Part | What the builder does |
//! |---|---|
//! | Mode | The partition's `default_mode` as the harness mode (`PartitionMode` → [`HarnessMode`](crate::HarnessMode)) |
//! | Identity | Every agent of the harness's runner acts as `urn:adk:agent:{app}/{agent}` with the partition's class and tenant (`Partition::identity_source`), so `PolicyRule::for_class` rules apply |
//! | Long-term facts | `FactScope.project` is the partition's `memory_scope`, for every kind, preferences included; recall and `memory_search` read that scope only, never a user-wide fact |
//! | Working notes | A memory root of the partition's own under the configured layout |
//! | Drivers | The drivers mounted for the partition ([`partition_drivers`](crate::HarnessAgentBuilder::partition_drivers)): a driver whose manifest lists the partition and whose id is in its `drivers`, classed by its manifest under the kernel's driver policy; any other driver is invisible |
//! | Crossings | The partition's `PartitionPolicy` composed with the operator policy (strictest wins); [`PARTITION_MEMORY_SEARCH`] and [`PARTITION_MEMORY_REMEMBER`] reach another partition's facts, and [`partition_crossing_tool`](crate::HarnessAgentBuilder::partition_crossing_tool) offers the coordinator another partition's driver or session tool; all of them are non-waivable |
//!
//! A crossing by an agent that is not the coordinator is denied at the policy. The
//! coordinator's crossing is held for a person: the prompt names it and offers "allow this
//! read of *Work › calendar* for this run", "deny" and "deny and block crossings to *Work*
//! from this partition" ([`CrossingPrompt`], [`CrossingAnswer`]). An approved read lets the
//! coordinator's identical reads through for the rest of that run; writes stay one-shot; no
//! standing rule and no expiring rule answers a crossing.

use std::sync::Arc;

use adk_core::{AdkError, Result, Tool, ToolConfirmationRequest, ToolPolicy, Toolset};
use adk_kernel::{
    Crossing, CrossingAccess, CrossingResource, CrossingRoute, CrossingTool, KernelBuilder,
    Partition, PartitionId, PartitionPolicy, Partitions,
};

use crate::approvals::{ApprovalPrompt, PromptAnswer};
use crate::harness::Harness;
use crate::policy::{ToolClassRegistry, ToolKind};

/// The tool that searches another partition's long-term facts (spec 3.2 §7.8): a read
/// crossing, class `Read`.
pub const PARTITION_MEMORY_SEARCH: &str = "partition_memory_search";
/// The tool that proposes a fact into another partition's long-term memory (spec 3.2 §7.8):
/// a write crossing, class `MemoryPropose`.
pub const PARTITION_MEMORY_REMEMBER: &str = "partition_memory_remember";

/// The session state key naming the partition whose harness ran the session (spec 3.2
/// §7.8, K-I15): a partition's harness writes it on its first run of a session and refuses a
/// session another partition's harness ran, and a consolidation tick distils only its own
/// partition's sessions.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::PARTITION_STATE_KEY, "harness:partition");
/// ```
pub const PARTITION_STATE_KEY: &str = "harness:partition";

/// The error code of a run a partition's harness refuses because another partition's
/// harness ran its session (spec 3.2 K-I15).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::PARTITION_SESSION_CODE, "harness.partition_session");
/// ```
pub const PARTITION_SESSION_CODE: &str = "harness.partition_session";

/// The kernel target of the harness `base` (the app's target, or `harness.goal`) in
/// `partition`: `<base>@<partition>` (spec 3.2 §7.8).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::partition_target("adk-rust", &"work".into()), "adk-rust@work");
/// ```
pub fn partition_target(base: &str, partition: &PartitionId) -> String {
    format!("{base}@{partition}")
}

/// The start of every crossing tool name into `to`: `<to>__`, with the characters a tool
/// name may not hold as `_`, as `CrossingTool` names its tools.
fn crossing_prefix(to: &PartitionId) -> String {
    format!("{to}__")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') { c } else { '_' })
        .collect()
}

/// A tool of another partition the coordinator may call as a crossing.
pub(crate) struct CrossingSpec {
    pub(crate) tool: Arc<dyn Tool>,
    pub(crate) to: PartitionId,
    pub(crate) resource: CrossingResource,
    pub(crate) access: CrossingAccess,
    pub(crate) name: String,
}

/// What [`HarnessAgentBuilder::partition`](crate::HarnessAgentBuilder::partition) and its
/// companions recorded.
#[derive(Default)]
pub(crate) struct PartitionOptions {
    pub(crate) partition: Option<(Partition, Partitions)>,
    pub(crate) coordinator: bool,
    pub(crate) crossings: Vec<CrossingSpec>,
    /// The kernel's drivers, mounted for the partition at build.
    pub(crate) drivers: Option<adk_kernel::drivers::DriverSupervisor>,
    /// The drivers of other partitions offered to the coordinator as crossings.
    pub(crate) driver_crossings: Option<(adk_kernel::drivers::DriverSupervisor, Vec<PartitionId>)>,
}

/// A partition's wiring and the coordinator's crossing tools, as the builder takes them.
pub(crate) struct WiredPartition {
    pub(crate) wiring: PartitionWiring,
    pub(crate) crossings: Vec<Arc<dyn Tool>>,
    /// Crossing tools listed at request time: other partitions' drivers.
    pub(crate) toolsets: Vec<Arc<dyn Toolset>>,
    /// `<to>__` for each partition those toolsets cross into, so no run-wide approval
    /// answers one of their tools (K-I15).
    pub(crate) prefixes: Vec<String>,
}

/// A partition's harness as built (spec 3.2 §7.8): its record and its policy.
#[derive(Clone, Debug)]
pub(crate) struct PartitionWiring {
    pub(crate) partition: Partition,
    pub(crate) policy: PartitionPolicy,
}

impl PartitionOptions {
    /// The partition's policy, with its memory crossing tools registered as crossings and
    /// as harness classes, and the coordinator's crossing tools wrapped; `None` for a
    /// harness built without a partition.
    ///
    /// # Errors
    ///
    /// A configuration error when a coordinator or a crossing tool is set without a
    /// partition.
    pub(crate) fn wire(
        self,
        agent: &str,
        registry: &mut ToolClassRegistry,
    ) -> Result<Option<WiredPartition>> {
        let Some((partition, partitions)) = self.partition else {
            if self.coordinator
                || !self.crossings.is_empty()
                || self.drivers.is_some()
                || self.driver_crossings.is_some()
            {
                return Err(AdkError::config(
                    "partition_coordinator, partition_crossing_tool, partition_drivers and \
                     partition_driver_crossings configure a partition's harness; set the \
                     partition with HarnessAgentBuilder::partition (spec 3.2 §7.8)",
                ));
            }
            return Ok(None);
        };
        if self.driver_crossings.is_some() && !self.coordinator {
            return Err(AdkError::config(
                "partition_driver_crossings offers other partitions' drivers to the \
                 coordinator, the one agent that may cross (spec 3.2 §7.8, K-I15); call \
                 partition_coordinator on this partition's harness, or leave the crossings out",
            ));
        }
        let mut policy = partitions.policy(partition.id.clone());
        if self.coordinator {
            policy = policy.with_coordinator(agent);
        }
        policy.register(
            PARTITION_MEMORY_SEARCH,
            CrossingRoute::argument(CrossingResource::Memory, CrossingAccess::Read, "memory"),
        );
        policy.register(
            PARTITION_MEMORY_REMEMBER,
            CrossingRoute::argument(CrossingResource::Memory, CrossingAccess::Write, "memory"),
        );
        registry.register(PARTITION_MEMORY_SEARCH, ToolKind::Read);
        registry.register(PARTITION_MEMORY_REMEMBER, ToolKind::MemoryPropose);
        let crossings = self
            .crossings
            .into_iter()
            .map(|spec| {
                Arc::new(CrossingTool::new(
                    spec.tool,
                    &policy,
                    spec.to,
                    spec.resource,
                    spec.access,
                    spec.name,
                )) as Arc<dyn Tool>
            })
            .collect();
        let (toolsets, prefixes) = match self.driver_crossings {
            Some((drivers, targets)) => {
                let targets: Vec<PartitionId> =
                    targets.into_iter().filter(|to| *to != partition.id).collect();
                let prefixes = targets.iter().map(crossing_prefix).collect();
                (vec![drivers.crossing_toolset(&policy, targets)], prefixes)
            }
            None => (Vec::new(), Vec::new()),
        };
        Ok(Some(WiredPartition {
            wiring: PartitionWiring { partition, policy },
            crossings,
            toolsets,
            prefixes,
        }))
    }
}

impl PartitionWiring {
    /// `existing` made stricter by the partition's policy (spec 3.2 K-I6): the operator
    /// layer of the harness.
    pub(crate) fn operator_policy(
        &self,
        existing: Option<Arc<dyn ToolPolicy>>,
    ) -> Arc<dyn ToolPolicy> {
        let partition: Arc<dyn ToolPolicy> = Arc::new(self.policy.clone());
        match existing {
            Some(existing) => Arc::new(adk_core::StrictestPolicy::new(vec![existing, partition])),
            None => partition,
        }
    }

    /// The memory tools that reach other partitions, over the harness's long-term facts.
    #[cfg(feature = "memory")]
    pub(crate) fn memory_tools(
        &self,
        facts: &Arc<crate::facts::runtime::FactsRuntime>,
    ) -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(memory::PartitionMemorySearchTool::new(facts.clone(), self.policy.clone())),
            Arc::new(memory::PartitionMemoryRememberTool::new(facts.clone(), self.policy.clone())),
        ]
    }

    /// The names of every crossing tool: no run-wide approval answers them (spec 3.2 K-I15).
    pub(crate) fn non_waivable(&self, crossings: &[Arc<dyn Tool>]) -> Vec<String> {
        [PARTITION_MEMORY_SEARCH.to_string(), PARTITION_MEMORY_REMEMBER.to_string()]
            .into_iter()
            .chain(crossings.iter().map(|tool| tool.name().to_string()))
            .collect()
    }

    /// `config` with a memory root of the partition's own, so no other partition's
    /// harness reads its working notes (spec 3.2 K-I15).
    #[cfg(feature = "memory")]
    pub(crate) fn scope_notes(
        &self,
        config: crate::memory::MemoryConfig,
        workspace_root: &std::path::Path,
    ) -> crate::memory::MemoryConfig {
        let scope = path_segment(&self.partition.memory_scope);
        let base = match &config.layout {
            crate::memory::MemoryLayout::PerUser { base } => base.join("partitions").join(scope),
            crate::memory::MemoryLayout::Project => {
                workspace_root.join(".adk").join("memory").join("partitions").join(scope)
            }
        };
        crate::memory::MemoryConfig {
            layout: crate::memory::MemoryLayout::PerUser { base },
            ..config
        }
    }

    /// The event that marks `ctx`'s session as this partition's on the harness's first run
    /// of it, or `None` when it already is (spec 3.2 K-I15).
    ///
    /// # Errors
    ///
    /// `Forbidden` ([`PARTITION_SESSION_CODE`]) when another partition's harness ran the
    /// session.
    pub(crate) fn session_mark(
        &self,
        ctx: &dyn adk_core::InvocationContext,
        author: &str,
    ) -> Result<Option<adk_core::Event>> {
        let id = self.partition.id.as_str();
        match ctx.session().state().get(PARTITION_STATE_KEY) {
            Some(owner) if owner.as_str() == Some(id) => Ok(None),
            Some(owner) => Err(AdkError::new(
                adk_core::ErrorComponent::Agent,
                adk_core::ErrorCategory::Forbidden,
                PARTITION_SESSION_CODE,
                format!(
                    "session '{}' belongs to partition {owner}; the harness of partition '{id}' \
                     cannot continue it (spec 3.2 K-I15); start a new session",
                    ctx.session_id()
                ),
            )),
            None => {
                let mut event = adk_core::Event::new(ctx.invocation_id());
                event.author = author.to_string();
                event.branch = ctx.branch().to_string();
                event
                    .actions
                    .state_delta
                    .insert(PARTITION_STATE_KEY.to_string(), serde_json::json!(id));
                Ok(Some(event))
            }
        }
    }

    /// The crossing `request` makes, or `None` when it reaches no other partition.
    pub(crate) fn crossing_of(&self, request: &ToolConfirmationRequest) -> Option<Crossing> {
        let route = self.policy.route(&request.tool_name)?;
        let crossing = route.crossing(self.policy.home(), &request.args).ok()?;
        (crossing.to != *self.policy.home()).then_some(crossing)
    }
}

/// The after-agent callback releasing the grants of the invocation that `root` ran, once
/// it ends (spec 3.2 §13 Q3).
pub(crate) fn release_on_end(
    policy: PartitionPolicy,
    root: String,
) -> adk_core::AfterAgentCallback {
    Box::new(move |ctx| {
        let policy = policy.clone();
        let root = root.clone();
        Box::pin(async move {
            if ctx.agent_name() == root {
                let run = adk_kernel::CrossingRun::Invocation(ctx.invocation_id().to_string());
                if let Err(error) = policy.partitions().release_run(&run).await {
                    tracing::warn!(error = %error, "could not release the invocation's crossing grants");
                }
            }
            Ok(None)
        })
    })
}

/// `text` as one path segment: itself when it is a plain name, its digest otherwise.
#[cfg(feature = "memory")]
fn path_segment(text: &str) -> String {
    let plain = !text.is_empty()
        && text != "."
        && text != ".."
        && text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if plain { text.to_string() } else { crate::fsutil::hex_digest(text.as_bytes()) }
}

/// A prompt for a partition crossing (spec 3.2 §7.8): the held call, the crossing it
/// makes, and the three answers the person picks from.
///
/// # Example
///
/// ```rust,ignore
/// let [allow, deny, block] = &prompt.answers;
/// println!("{}\n1) {allow}\n2) {deny}\n3) {block}", prompt.call.rendered.title);
/// ```
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct CrossingPrompt {
    /// The held call, rendered as every prompt is.
    pub call: ApprovalPrompt,
    /// Where the call reaches.
    pub crossing: Crossing,
    /// "allow this read of *X* for this run" (or "allow this change to *X* once"), "deny",
    /// and "deny and block crossings to *X* from this partition".
    pub answers: [String; 3],
}

impl CrossingPrompt {
    /// A prompt for `call`, which makes `crossing`.
    pub fn new(call: ApprovalPrompt, crossing: Crossing) -> Self {
        let answers = crossing.answers();
        Self { call, crossing, answers }
    }
}

/// A person's answer to a [`CrossingPrompt`] (spec 3.2 §7.8).
///
/// # Example
///
/// ```rust
/// use adk_harness::CrossingAnswer;
///
/// assert_ne!(CrossingAnswer::Allow, CrossingAnswer::DenyAndBlock);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CrossingAnswer {
    /// Allow it: a read for the rest of the run, anything else once.
    Allow,
    /// Do not run it.
    Deny {
        /// Reported to the model.
        message: Option<String>,
    },
    /// Do not run it, and block crossings to its partition from this one.
    DenyAndBlock,
}

impl CrossingAnswer {
    /// The answer a plain [`ApprovalPrompter::prompt`](crate::ApprovalPrompter::prompt)
    /// gives for a crossing: an approval allows it, an approval with other arguments is
    /// refused (a crossing runs as asked or not at all), a denial denies it, and a denial
    /// with a remembered rule blocks the partition, since no rule answers a crossing.
    pub fn from_prompt_answer(answer: PromptAnswer) -> Self {
        match answer {
            PromptAnswer::ApproveOnce | PromptAnswer::ApproveAndRemember(_) => Self::Allow,
            PromptAnswer::ApproveWithArgs(_) => Self::Deny {
                message: Some(
                    "a partition crossing runs with the arguments it was asked with or not at all"
                        .into(),
                ),
            },
            PromptAnswer::Deny { message } => Self::Deny { message },
            PromptAnswer::DenyAndRemember(_) => Self::DenyAndBlock,
            PromptAnswer::DenyAllRemaining => Self::Deny { message: None },
        }
    }
}

impl Harness {
    /// The partition this harness was built for (spec 3.2 §7.8).
    pub fn partition(&self) -> Option<&Partition> {
        self.agent().partition()
    }

    /// Hosts this partition's harness on `kernel` (spec 3.2 §7.8): its runner as the
    /// targets `<app>@<partition>` and `harness.goal@<partition>`, bound to the partition,
    /// so a process of another partition cannot run it; its `propose_followup` on the
    /// kernel's agenda; and its progress source for the partition's processes
    /// (`KernelBuilder::partition_progress_source`), so the kernel reads their goal progress,
    /// loop-guard verdicts and usage from it and its ledger rows carry the partition. A
    /// harness built without a partition is returned `kernel` unchanged. Hosting a partition
    /// twice fails at `build`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let kernel = home.kernel().partitions(partitions.clone());
    /// let kernel = work.host_partition(kernel).build()?;
    /// ```
    pub fn host_partition(&self, kernel: KernelBuilder) -> KernelBuilder {
        match self.agent().partition_wiring() {
            Some(_) => self.host_partition_with(kernel, self.kernel_progress()),
            None => kernel,
        }
    }

    /// [`host_partition`](Self::host_partition) with `progress` as the partition's
    /// progress source.
    pub(crate) fn host_partition_with(
        &self,
        kernel: KernelBuilder,
        progress: Arc<crate::kernel::HarnessProgress>,
    ) -> KernelBuilder {
        let Some(wiring) = self.agent().partition_wiring() else {
            return kernel;
        };
        let id = wiring.partition.id.clone();
        let (goal, runner) = self.goal_target();
        kernel
            .partition_agent(id.clone(), partition_target(self.app_name(), &id), runner.clone())
            .partition_agent(id.clone(), goal, runner)
            .partition_progress_source(id, progress)
            .partitions(wiring.policy.partitions().clone())
            .agenda_handle(self.agent().runtime().kernel_agenda.clone())
    }
}

#[cfg(feature = "memory")]
mod memory {
    //! The memory crossing tools (spec 3.2 §7.8).

    use std::sync::Arc;

    use adk_core::{
        AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolContext, ToolEffect,
    };
    use adk_kernel::{CROSSING_PARTITION_ARG, PartitionId, PartitionPolicy};
    use async_trait::async_trait;
    use serde_json::{Value, json};

    use super::{PARTITION_MEMORY_REMEMBER, PARTITION_MEMORY_SEARCH};
    use crate::audit::HarnessAudit;
    use crate::facts::runtime::FactsRuntime;
    use crate::facts::writer::{AdmitContext, Admitted, FactCandidate};
    use crate::facts::{FactKind, FactScope};
    use crate::mode::HarnessMode;
    use crate::policy::has_harness_layer;
    use crate::tools::facts::{REMEMBER_CONFIDENCE, SEARCH_MAX, describe};

    fn invalid(message: impl Into<String>) -> AdkError {
        AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::InvalidInput,
            "harness.memory.invalid_args",
            message,
        )
    }

    fn unavailable(message: &str) -> AdkError {
        AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::Forbidden,
            "harness.memory.unavailable",
            message,
        )
    }

    fn target(args: &Value) -> Result<PartitionId> {
        args.get(CROSSING_PARTITION_ARG)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|partition| !partition.is_empty())
            .map(PartitionId::new)
            .ok_or_else(|| invalid("name the partition to reach in 'partition'"))
    }

    /// The memory scope of the partition `id`.
    async fn scope_of(policy: &PartitionPolicy, id: &PartitionId) -> Result<String> {
        match policy.partitions().get(id).await {
            Ok(Some(partition)) => Ok(partition.memory_scope),
            Ok(None) => Err(invalid(format!("there is no partition named `{id}`"))),
            Err(error) => Err(error.into()),
        }
    }

    /// `partition_memory_search`: another partition's long-term facts matching a query
    /// (spec 3.2 §7.8; class `Read`, a read crossing).
    pub(super) struct PartitionMemorySearchTool {
        facts: Arc<FactsRuntime>,
        policy: PartitionPolicy,
    }

    impl PartitionMemorySearchTool {
        pub(super) fn new(facts: Arc<FactsRuntime>, policy: PartitionPolicy) -> Self {
            Self { facts, policy }
        }
    }

    #[async_trait]
    impl Tool for PartitionMemorySearchTool {
        fn name(&self) -> &str {
            PARTITION_MEMORY_SEARCH
        }

        fn description(&self) -> &str {
            "Search what is remembered in another partition, such as work from home. Only \
             the coordinator may reach another partition, and every crossing needs the \
             user's approval; for this partition's own facts use memory_search."
        }

        fn parameters_schema(&self) -> Option<Value> {
            Some(json!({
                "type": "object",
                "properties": {
                    "partition": { "type": "string", "description": "The partition to search, such as `work`." },
                    "query": { "type": "string", "description": "What to look for." },
                    "kind": { "type": "string", "enum": ["preference", "project_fact", "procedure", "outcome", "decision", "correction"], "description": "Only facts of this kind." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": SEARCH_MAX, "description": "Most facts to return (default 8)." }
                },
                "required": ["partition", "query"]
            }))
        }

        fn is_read_only(&self) -> bool {
            true
        }

        fn effect(&self) -> ToolEffect {
            ToolEffect::ReadOnly
        }

        async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
            if !self.facts.serves(ctx.as_ref()) {
                return Err(unavailable(
                    "long-term memory serves the harness's own agent for an authenticated user",
                ));
            }
            let to = target(&args)?;
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("partition_memory_search needs a string 'query'"))?
                .to_string();
            let kind = match args.get("kind").and_then(Value::as_str) {
                Some(kind) => Some(kind.parse::<FactKind>()?),
                None => None,
            };
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .map_or(self.facts.config.recall_limit, |limit| limit as usize)
                .clamp(1, SEARCH_MAX);
            let body = || async {
                let project = scope_of(&self.policy, &to).await?;
                let scope = FactScope::new(ctx.app_name(), ctx.user_id(), Some(project));
                let recalled = self.facts.recall(&scope, &query, kind, limit, None).await?;
                let audit = HarnessAudit::for_tool(ctx.as_ref());
                self.facts.record_recalled(audit.as_ref(), &recalled, &query).await?;
                Ok(json!({
                    "partition": to,
                    "facts": recalled.facts.iter().map(|(fact, _)| describe(fact)).collect::<Vec<_>>(),
                }))
            };
            self.policy.run_crossing(ctx.as_ref(), self, &args, body).await
        }
    }

    /// `partition_memory_remember`: proposes a fact into another partition's long-term
    /// memory, always `Assumed` (spec 3.2 §7.8; class `MemoryPropose`, a write crossing).
    pub(super) struct PartitionMemoryRememberTool {
        facts: Arc<FactsRuntime>,
        policy: PartitionPolicy,
    }

    impl PartitionMemoryRememberTool {
        pub(super) fn new(facts: Arc<FactsRuntime>, policy: PartitionPolicy) -> Self {
            Self { facts, policy }
        }
    }

    #[async_trait]
    impl Tool for PartitionMemoryRememberTool {
        fn name(&self) -> &str {
            PARTITION_MEMORY_REMEMBER
        }

        fn description(&self) -> &str {
            "Remember a fact in another partition, such as a work decision taken from home. \
             Only the coordinator may reach another partition, and every write needs the \
             user's approval; for this partition use memory_remember."
        }

        fn parameters_schema(&self) -> Option<Value> {
            Some(json!({
                "type": "object",
                "properties": {
                    "partition": { "type": "string", "description": "The partition to write to, such as `work`." },
                    "text": { "type": "string", "description": "One fact, at most 500 characters." },
                    "kind": { "type": "string", "enum": ["preference", "project_fact", "procedure", "outcome", "decision", "correction"] }
                },
                "required": ["partition", "text", "kind"]
            }))
        }

        fn effect(&self) -> ToolEffect {
            ToolEffect::Idempotent
        }

        async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
            let governed = ctx
                .run_config()
                .and_then(|config| config.tool_policy.as_deref())
                .is_some_and(has_harness_layer);
            if !governed {
                return Err(unavailable(
                    "partition_memory_remember runs only inside a harness run, under the harness policy",
                ));
            }
            if !self.facts.serves(ctx.as_ref()) {
                return Err(unavailable(
                    "long-term memory serves the harness's own agent for an authenticated user",
                ));
            }
            let mode = self.facts.mode(ctx.app_name(), ctx.user_id(), ctx.session_id()).await;
            if matches!(mode, HarnessMode::ReadOnly | HarnessMode::Plan) {
                return Err(unavailable("read-only and plan modes write no facts"));
            }
            let to = target(&args)?;
            let candidate = FactCandidate {
                text: args
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("partition_memory_remember needs a string 'text'"))?
                    .to_string(),
                kind: args
                    .get("kind")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("partition_memory_remember needs a string 'kind'"))?
                    .to_string(),
                source: Some("agent_inferred".to_string()),
                quote: None,
                confidence: Some(REMEMBER_CONFIDENCE),
                supersedes: None,
            };
            let body = || async {
                let project = scope_of(&self.policy, &to).await?;
                let context = AdmitContext {
                    app: ctx.app_name(),
                    user: ctx.user_id(),
                    session_id: ctx.session_id(),
                    invocation_id: ctx.invocation_id(),
                    run_id: ctx.run_config().and_then(|config| config.durable_run_id.clone()),
                    user_messages: &[],
                    audit: HarnessAudit::for_tool(ctx.as_ref()),
                    tick: None,
                };
                Ok(match self.facts.admit_into(&candidate, &context, Some(&project)).await? {
                    Admitted::Stored(fact) => {
                        json!({ "partition": to, "remembered": describe(&fact) })
                    }
                    Admitted::Duplicate(fact) => {
                        json!({ "partition": to, "remembered": describe(&fact), "alreadyKnown": true })
                    }
                    Admitted::Rejected { reason, .. } => {
                        json!({ "error": format!("the fact was not remembered: {reason}") })
                    }
                })
            };
            self.policy.run_crossing(ctx.as_ref(), self, &args, body).await
        }
    }
}

#[cfg(all(test, feature = "memory"))]
mod tests {
    use std::sync::Arc;

    use proptest::prelude::*;

    use crate::facts::runtime::FactsRuntime;
    use crate::facts::writer::{AdmitContext, Admitted, FactCandidate};
    use crate::facts::{FactScope, InMemoryFactStore, LocalFactIndex, LongTermMemoryConfig};

    const KINDS: [&str; 6] =
        ["preference", "project_fact", "procedure", "outcome", "decision", "correction"];

    /// A harness's long-term memory over `store`: a partition's (confined to `project`) or,
    /// unconfined, a harness built without a partition.
    fn runtime(store: &Arc<InMemoryFactStore>, project: &str, confined: bool) -> FactsRuntime {
        let model =
            Arc::new(adk_model::ScriptedLlm::new("m", Vec::<adk_model::ScriptedResponse>::new()));
        FactsRuntime::new(
            LongTermMemoryConfig::default(),
            store.clone(),
            Arc::new(LocalFactIndex::new(store.clone())),
            project.into(),
            "harness".into(),
            false,
            None,
            model,
            None,
            Arc::new(crate::mode::InMemoryModeStore::new()),
            crate::mode::HarnessMode::Ask,
        )
        .confined(confined)
    }

    async fn admit(
        runtime: &FactsRuntime,
        text: &str,
        kind: &str,
        target: Option<&str>,
    ) -> Admitted {
        let context = AdmitContext {
            app: "app",
            user: "alice",
            session_id: "s1",
            invocation_id: "inv-1",
            run_id: None,
            user_messages: &[],
            audit: None,
            tick: None,
        };
        let candidate =
            FactCandidate { text: text.into(), kind: kind.into(), ..FactCandidate::default() };
        runtime.admit_into(&candidate, &context, target).await.unwrap()
    }

    async fn recalled(runtime: &FactsRuntime, project: &str, query: &str) -> Vec<FactScope> {
        let scope = FactScope::new("app", "alice", Some(project.into()));
        let recalled = runtime.recall(&scope, query, None, 50, None).await.unwrap();
        recalled.facts.into_iter().map(|(fact, _)| fact.scope).collect()
    }

    #[tokio::test]
    async fn a_partitions_preference_stays_in_its_scope_and_a_user_wide_one_is_not_recalled() {
        let store = Arc::new(InMemoryFactStore::new());
        let home = runtime(&store, "home", true);
        let plain = runtime(&store, "repo", false);
        let Admitted::Stored(fact) =
            admit(&home, "prefers morning meetings", "preference", None).await
        else {
            panic!("stored");
        };
        assert_eq!(fact.scope.project.as_deref(), Some("home"), "not user-wide in a partition");
        admit(&plain, "prefers dark roast coffee", "preference", None).await;
        assert!(recalled(&home, "home", "coffee").await.is_empty());
        assert_eq!(
            recalled(&plain, "repo", "coffee").await.len(),
            1,
            "unchanged without a partition"
        );
        assert_eq!(recalled(&home, "home", "meetings").await.len(), 1);
    }

    #[tokio::test]
    async fn a_partition_cannot_supersede_another_partitions_fact_by_id() {
        let store = Arc::new(InMemoryFactStore::new());
        let (home, work) = (runtime(&store, "home", true), runtime(&store, "work", true));
        let Admitted::Stored(fact) =
            admit(&work, "the build runs nightly", "project_fact", None).await
        else {
            panic!("stored");
        };
        let context = AdmitContext {
            app: "app",
            user: "alice",
            session_id: "s1",
            invocation_id: "inv-1",
            run_id: None,
            user_messages: &[],
            audit: None,
            tick: None,
        };
        let candidate = FactCandidate {
            text: "the build runs hourly".into(),
            kind: "project_fact".into(),
            supersedes: Some(fact.id.clone()),
            ..FactCandidate::default()
        };
        let admitted = home.admit(&candidate, &context).await.unwrap();
        assert!(matches!(admitted, Admitted::Rejected { .. }), "{admitted:?}");
    }

    #[test]
    fn each_partition_keeps_working_notes_of_its_own() {
        let partitions = adk_kernel::Partitions::in_memory();
        let wiring = |id: &str| super::PartitionWiring {
            partition: adk_kernel::Partition::new(id, id),
            policy: partitions.policy(id),
        };
        let workspace = std::path::Path::new("/work/repo");
        let per_user = crate::memory::MemoryConfig {
            layout: crate::memory::MemoryLayout::PerUser { base: "/m".into() },
            ..crate::memory::MemoryConfig::default()
        };
        let roots: Vec<_> = ["home", "work", "a b/c"]
            .iter()
            .map(|id| {
                wiring(id)
                    .scope_notes(per_user.clone(), workspace)
                    .root_for("app", "alice", workspace)
            })
            .collect();
        assert!(roots[0].starts_with("/m/partitions/home"), "{:?}", roots[0]);
        assert!(roots[1].starts_with("/m/partitions/work"), "{:?}", roots[1]);
        assert_ne!(roots[0], roots[1]);
        let digest = roots[2].strip_prefix("/m/partitions").unwrap().components().next().unwrap();
        assert_eq!(digest.as_os_str().len(), 64, "a scope that is not a plain name is digested");
        let project = crate::memory::MemoryConfig {
            layout: crate::memory::MemoryLayout::Project,
            ..crate::memory::MemoryConfig::default()
        };
        let root =
            wiring("home").scope_notes(project, workspace).root_for("app", "alice", workspace);
        assert!(root.starts_with("/work/repo/.adk/memory/partitions/home"), "{root:?}");
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        /// No recall or memory search crosses partitions (spec 3.2 K-I15): whatever three
        /// partitions and a harness without a partition write into one fact store, a
        /// partition recalls only the facts of its own scope.
        #[test]
        fn no_recall_or_memory_search_crosses_partitions(
            writes in proptest::collection::vec(
                (0usize..4, 0usize..KINDS.len(), "[a-z]{3,8}( [a-z]{3,8}){1,4}"),
                1..14,
            ),
            queries in proptest::collection::vec("[a-z]{3,8}( [a-z]{3,8}){0,2}", 1..5),
        ) {
            let runtime_ = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            runtime_.block_on(async {
                let store = Arc::new(InMemoryFactStore::new());
                let projects = ["home", "work", "team"];
                let partitions: Vec<FactsRuntime> =
                    projects.iter().map(|project| runtime(&store, project, true)).collect();
                let plain = runtime(&store, "repo", false);
                let mut texts = Vec::new();
                let mut stored: Vec<Vec<String>> = vec![Vec::new(); projects.len()];
                for (writer, kind, text) in &writes {
                    let runtime = partitions.get(*writer).unwrap_or(&plain);
                    if let Admitted::Stored(_) = admit(runtime, text, KINDS[*kind], None).await
                        && *writer < projects.len()
                    {
                        stored[*writer].push(text.clone());
                    }
                    texts.push(text.clone());
                }
                // Not vacuous: each partition finds what it wrote.
                for (index, partition) in partitions.iter().enumerate() {
                    for text in &stored[index] {
                        prop_assert!(!recalled(partition, projects[index], text).await.is_empty());
                    }
                }
                for (index, partition) in partitions.iter().enumerate() {
                    for query in queries.iter().chain(texts.iter()) {
                        for scope in recalled(partition, projects[index], query).await {
                            prop_assert_eq!(scope.project.as_deref(), Some(projects[index]));
                        }
                    }
                }
                Ok(())
            })?;
        }
    }
}
