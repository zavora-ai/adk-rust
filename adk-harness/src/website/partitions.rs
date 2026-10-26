//! The two partitions of a site and the agents built in them (spec 3.5 §7.10, §7.19, D-5,
//! A-I2, A-I3, A-I22).
//!
//! | | Public | Back office |
//! |---|---|---|
//! | Partition | `{site}.public`, class `website.public`, read-only mode | `{site}.back-office`, class `website.back-office`, plan mode until the charter compiles to more |
//! | Principals | `urn:adk:agent:{site}/{agent}` | `urn:adk:agent:{site}/{agent}` |
//! | Memory scope | `{site}/public`: no owner facts | `{site}/back-office`: the owner's facts, the charter |
//! | Tools | declared capabilities, `search_site`, `reply_in_thread`, `emit_signal` | the back-office toolset |
//! | Policy | [`PublicPolicy`]: the manifest's fixed list, no standing rules | the partition policy, [`WebsitePolicy`], the ladder and job pauses, `adk-drivers`' caps |
//!
//! Neither partition crosses into the other: a signal is not a crossing, and a back-office
//! request for the raw text behind a signal (`signal_source_text`) is a crossing into the
//! public partition's sessions, which the public partition refuses.

use std::sync::Arc;

use adk_core::audit_log::AuditActor;
use adk_core::{Result, Tool, ToolContext, ToolEffect};
use adk_kernel::charter::{AutonomyStore, JobPauses, SignalSink, SignalSourceTool, SignalStore};
use adk_kernel::{AgendaStore, CrossPartition, Partition, PartitionMode, Partitions};
use async_trait::async_trait;
use serde_json::Value;

use super::back_office::back_office_tools;
use super::catalog::PUBLIC_ORIGIN;
use super::ladder::LadderCompiler;
use super::policy::WebsitePolicy;
use super::public::{EmitSignalTool, PublicPolicy};
use super::search::{SearchSiteTool, SiteSearch};
use super::site::{SiteRef, SiteRuntime, kernel_error};
use crate::builder::HarnessAgentBuilder;
use crate::facts::FactScope;

/// `AgentPrincipal.class` of the public partition's agents.
pub const PUBLIC_CLASS: &str = "website.public";
/// `AgentPrincipal.class` of the back office's agents.
pub const BACK_OFFICE_CLASS: &str = "website.back-office";

/// A site's public and back-office partitions (spec 3.5 §7.10).
///
/// Each partition's tenant is the site, so in agency mode every site's agents carry their
/// own site as tenant and no partition of one site is another's (A-I22).
///
/// # Example
///
/// ```rust
/// use adk_harness::website::{SitePartitions, SiteRef};
///
/// let partitions = SitePartitions::new(&SiteRef::new(uuid::Uuid::nil(), "bakery", "https://bakery.example"));
/// assert_eq!(partitions.public().id.as_str(), "bakery.public");
/// assert_eq!(partitions.back_office().memory_scope, "bakery/back-office");
/// let principal = partitions.back_office().principal("bakery", "steward");
/// assert_eq!(principal.id.as_str(), "urn:adk:agent:bakery/steward");
/// assert_eq!(principal.tenant.as_deref(), Some("bakery"));
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct SitePartitions {
    site: SiteRef,
    public: Partition,
    back_office: Partition,
}

impl SitePartitions {
    /// The partitions of `site`.
    pub fn new(site: &SiteRef) -> Self {
        let public = Partition::new(format!("{}.public", site.name), PUBLIC_CLASS)
            .with_tenant(site.name.clone())
            .with_memory_scope(format!("{}/public", site.name))
            .with_mode(PartitionMode::ReadOnly)
            .with_cross_partition(CrossPartition::Deny);
        let back_office = Partition::new(format!("{}.back-office", site.name), BACK_OFFICE_CLASS)
            .with_tenant(site.name.clone())
            .with_memory_scope(format!("{}/back-office", site.name))
            .with_mode(PartitionMode::Plan)
            .with_cross_partition(CrossPartition::Deny);
        Self { site: site.clone(), public, back_office }
    }

    /// The site.
    pub fn site(&self) -> &SiteRef {
        &self.site
    }

    /// The public partition.
    pub fn public(&self) -> &Partition {
        &self.public
    }

    /// The back-office partition.
    pub fn back_office(&self) -> &Partition {
        &self.back_office
    }

    /// Writes both partitions into the kernel's table.
    ///
    /// # Errors
    ///
    /// The table's error.
    pub async fn install(&self, table: &Partitions, by: AuditActor) -> Result<()> {
        table.put(self.public.clone(), by.clone()).await.map_err(kernel_error)?;
        table.put(self.back_office.clone(), by).await.map_err(kernel_error)
    }

    /// The scope of the owner `user`'s facts in the back office: the facts claims are
    /// checked against (A-I18).
    pub fn fact_scope(&self, user: &str) -> FactScope {
        FactScope::new(self.site.name.clone(), user, Some(self.back_office.memory_scope.clone()))
    }

    /// The sink public agents file signals into: the back office's agenda (spec 3.5 §7.10).
    pub fn signal_sink(
        &self,
        store: Arc<dyn SignalStore>,
        agenda: Arc<dyn AgendaStore>,
    ) -> SignalSink {
        SignalSink::new(store, agenda, self.back_office.id.clone())
    }
}

/// A declared capability served by the public partition: the capability tool, published as
/// the site's (spec 3.5 §7.4, §7.10), so the manifest policy decides it.
struct PublicCapability {
    inner: Arc<dyn Tool>,
}

#[async_trait]
impl Tool for PublicCapability {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn declaration(&self) -> Value {
        self.inner.declaration()
    }

    fn parameters_schema(&self) -> Option<Value> {
        self.inner.parameters_schema()
    }

    fn response_schema(&self) -> Option<Value> {
        self.inner.response_schema()
    }

    fn annotations(&self) -> Option<&Value> {
        self.inner.annotations()
    }

    fn origin(&self) -> &str {
        PUBLIC_ORIGIN
    }

    fn effect(&self) -> ToolEffect {
        self.inner.effect()
    }

    fn is_long_running(&self) -> bool {
        self.inner.is_long_running()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        self.inner.execute(ctx, args).await
    }
}

/// What a public agent is built with (spec 3.5 §7.10).
pub struct PublicKit {
    /// The site's partitions.
    pub partitions: SitePartitions,
    /// The kernel's partition table, holding both.
    pub table: Partitions,
    /// The current release's index.
    pub search: Arc<SiteSearch>,
    /// The back office's signal sink.
    pub signals: SignalSink,
    /// `adk-drivers`' `reply_in_thread` over the inbox drivers, for agents that answer
    /// threads (the Inbox).
    pub reply: Option<Arc<adk_drivers::ReplyInThread>>,
    /// The declared capabilities, each with whether the manifest holds its calls.
    pub capabilities: Vec<(Arc<dyn Tool>, bool)>,
}

/// The public partition's tools when another runtime serves them, such as the AWP edge's
/// `PublicTools` (spec 3.5 §7.9, §7.10): the agent is built over these instead of the kit's
/// own `search_site` and `emit_signal`, which the edge's public policy would refuse.
///
/// # Example
///
/// ```rust,ignore
/// let concierge = HarnessAgent::builder("concierge")
///     .model(model)
///     .workspace(workspace)
///     .website_public_served(ServedPublicTools {
///         partitions,
///         table,
///         search_site: edge_tools.search_site.clone(),
///         reply_in_thread: Some(edge_tools.reply_in_thread.clone()),
///         emit_signal: edge_tools.emit_signal.clone(),
///         capabilities: edge_tools.capabilities.iter().map(|tool| (tool.clone(), false)).collect(),
///     })
///     .build()?;
/// ```
pub struct ServedPublicTools {
    /// The site's partitions.
    pub partitions: SitePartitions,
    /// The kernel's partition table, holding both.
    pub table: Partitions,
    /// `search_site`; left out when a declared capability has that name.
    pub search_site: Arc<dyn Tool>,
    /// `reply_in_thread`, for agents that answer threads.
    pub reply_in_thread: Option<Arc<dyn Tool>>,
    /// `emit_signal`.
    pub emit_signal: Arc<dyn Tool>,
    /// The declared capabilities, each with whether the manifest holds its calls here; the
    /// runtime serving them may hold them on its own governed path too.
    pub capabilities: Vec<(Arc<dyn Tool>, bool)>,
}

/// Classes the served public tools by their own origin and name as the website preset
/// classes the kit's: the public partition's manifest policy is the one policy that decides
/// them (spec 3.5 §7.10 "Mode").
#[derive(Debug)]
struct ServedClasses {
    tools: std::collections::BTreeSet<(String, String)>,
}

impl crate::policy::OriginClasses for ServedClasses {
    fn class_of(&self, source: &str, tool: &str) -> Option<crate::policy::ToolKind> {
        self.tools
            .contains(&(source.to_string(), tool.to_string()))
            .then_some(crate::policy::ToolKind::Todo)
    }
}

/// What a back-office agent is built with (spec 3.5 §7.10, §7.11).
pub struct BackOfficeKit {
    /// The site's partitions.
    pub partitions: SitePartitions,
    /// The kernel's partition table, holding both.
    pub table: Partitions,
    /// The site runtime.
    pub site: Arc<SiteRuntime>,
    /// The site's jobs.
    pub ladder: LadderCompiler,
    /// The jobs' levels.
    pub autonomy: Arc<dyn AutonomyStore>,
    /// The jobs' pauses, which the KPI tracker applies (A-I15).
    pub pauses: Arc<JobPauses>,
    /// The owner's budgets for driver tools that act within budget.
    pub driver_budget: adk_drivers::DriverBudget,
}

impl BackOfficeKit {
    /// The kit, with the pauses of `ladder`'s jobs and no driver budget.
    pub fn new(
        partitions: SitePartitions,
        table: Partitions,
        site: Arc<SiteRuntime>,
        ladder: LadderCompiler,
        autonomy: Arc<dyn AutonomyStore>,
    ) -> Self {
        let pauses = Arc::new(ladder.job_pauses());
        Self {
            partitions,
            table,
            site,
            ladder,
            autonomy,
            pauses,
            driver_budget: adk_drivers::DriverBudget::new(),
        }
    }
}

impl HarnessAgentBuilder {
    /// Builds an agent of the site's public partition (spec 3.5 §7.10, A-I2): the website
    /// preset in the public partition — read-only mode, the site's tenant, the public memory
    /// scope — with `search_site`, `emit_signal`, `reply_in_thread` when the kit has one,
    /// the declared capabilities, and [`PublicPolicy`] denying everything else, whatever a
    /// model asks. The agent is offered those tools and nothing else: no workspace, memory
    /// or plan tool.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let concierge = HarnessAgent::builder("concierge")
    ///     .model(model)
    ///     .workspace(workspace)
    ///     .website_public(kit)
    ///     .build()?;
    /// ```
    pub fn website_public(self, kit: PublicKit) -> Self {
        let mut policy = PublicPolicy::new();
        let mut builder = self
            .preset(crate::Preset::Website)
            .partition(kit.partitions.public().clone(), kit.table.clone())
            .without_todos()
            .tool(Arc::new(SearchSiteTool::new(kit.search.clone())))
            .tool(Arc::new(EmitSignalTool::new(kit.signals.clone())));
        if let Some(reply) = kit.reply {
            builder = builder.tool(reply);
        }
        for (capability, held) in kit.capabilities {
            policy = policy.with_capability(capability.name(), held);
            builder = builder.tool(Arc::new(PublicCapability { inner: capability }));
        }
        let offered = policy.tools().map(str::to_string).collect();
        builder.add_operator_policy(Arc::new(policy)).public_only(offered)
    }

    /// Builds an agent of the site's public partition over tools another runtime serves,
    /// such as the AWP edge's `PublicTools` (spec 3.5 §7.9, §7.10, A-I2): the partition and
    /// policy of [`website_public`](Self::website_public), with the served `search_site`
    /// (unless a declared capability has that name), `reply_in_thread`, `emit_signal` and
    /// capabilities, each classed by its own origin so the manifest policy decides it. The
    /// agent is offered those tools and nothing else.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let concierge = HarnessAgent::builder("concierge")
    ///     .model(model)
    ///     .workspace(workspace)
    ///     .website_public_served(served)
    ///     .build()?;
    /// ```
    pub fn website_public_served(self, tools: ServedPublicTools) -> Self {
        let declared_search =
            tools.capabilities.iter().any(|(tool, _)| tool.name() == super::catalog::SEARCH_SITE);
        let mut policy = PublicPolicy::new();
        let mut chosen: Vec<Arc<dyn Tool>> = Vec::new();
        for (capability, held) in tools.capabilities {
            policy = policy.with_capability(capability.name(), held);
            chosen.push(capability);
        }
        if !declared_search {
            chosen.push(tools.search_site);
        }
        chosen.extend(tools.reply_in_thread);
        chosen.push(tools.emit_signal);
        let classes = ServedClasses {
            tools: chosen
                .iter()
                .map(|tool| (tool.origin().to_string(), tool.name().to_string()))
                .collect(),
        };
        let mut builder = self
            .preset(crate::Preset::Website)
            .partition(tools.partitions.public().clone(), tools.table.clone())
            .without_todos()
            .tool_classes(Arc::new(classes));
        for tool in chosen {
            builder = builder.tool(tool);
        }
        let offered = policy.tools().map(str::to_string).collect();
        builder.add_operator_policy(Arc::new(policy)).public_only(offered)
    }

    /// Builds an agent of the site's back office (spec 3.5 §7.10, §7.11): the website preset
    /// in the back-office partition with the back-office toolset, `signal_source_text`
    /// registered as a crossing into the public partition — denied — and, strictest wins,
    /// the partition policy, [`WebsitePolicy`]'s holds, the ladder over the jobs' levels
    /// with their pauses, and `adk-drivers`' caps, which hold every DNS change (A-I21).
    ///
    /// The partition starts the agent in plan mode; set the compiled mode after this call
    /// with `.mode(kit.ladder.mode_for_agent(..))`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let steward = HarnessAgent::builder("steward")
    ///     .model(model)
    ///     .workspace(workspace)
    ///     .website_back_office(&kit)
    ///     .mode(kit.ladder.mode_for_agent("steward", &levels))
    ///     .build()?;
    /// ```
    pub fn website_back_office(self, kit: &BackOfficeKit) -> Self {
        let back_office = kit.partitions.back_office();
        let crossings = kit.table.policy(back_office.id.clone());
        let signal_source = SignalSourceTool::new(&crossings, kit.partitions.public().id.clone());
        let mut builder =
            self.preset(crate::Preset::Website).partition(back_office.clone(), kit.table.clone());
        for tool in back_office_tools(&kit.site) {
            builder = builder.tool(tool);
        }
        builder
            .tool(Arc::new(signal_source))
            .add_operator_policy(Arc::new(crossings))
            .add_operator_policy(Arc::new(WebsitePolicy::new(kit.site.clone())))
            .add_operator_policy(kit.ladder.policy(kit.autonomy.clone(), kit.pauses.clone()))
            .add_operator_policy(Arc::new(adk_drivers::DriverCapPolicy::new(
                kit.driver_budget.clone(),
            )))
    }
}
