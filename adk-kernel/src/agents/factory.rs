//! How a host builds the agents a person adds (spec 3.3 §5.7, §10 K7).

use std::sync::Arc;

use adk_core::{AgentId, ApprovalRisk};
use adk_runner::Runner;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agents::record::{AgentDefinition, AgentRecord, AgentUses, ModelChoice};
use crate::error::Result;
use crate::partition::{PartitionId, PartitionMode};
use crate::process::ProgressSource;

/// What an agent definition lets it do, as a factory or the kernel assesses it (spec 3.3
/// §5.7): the risk of adding it, whether it can spend money, and its abilities in plain
/// sentences.
///
/// The risk is one of [`ApprovalRisk::Low`], [`Medium`](ApprovalRisk::Medium) and
/// [`High`](ApprovalRisk::High); "High, spends money" is `High` with
/// [`money`](Self::money).
///
/// # Example
///
/// ```rust
/// use adk_core::ApprovalRisk;
/// use adk_kernel::agents::AgentAssessment;
///
/// let low = AgentAssessment::new(ApprovalRisk::Low).with_ability("It can read files in Invoices.");
/// let high = AgentAssessment::new(ApprovalRisk::High).with_money();
/// let both = low.clone().merge(high);
/// assert_eq!(both.risk, ApprovalRisk::High);
/// assert!(both.money);
/// assert_eq!(both.abilities, ["It can read files in Invoices."]);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AgentAssessment {
    /// The risk of adding the agent, or of the change.
    pub risk: ApprovalRisk,
    /// Whether it can spend money: the payments or computer-use pack, or a driver that
    /// spends or publishes (K-I9).
    #[serde(default)]
    pub money: bool,
    /// What it can do, one plain sentence each.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub abilities: Vec<String>,
}

impl AgentAssessment {
    /// An assessment at `risk`, spending no money, with no abilities listed.
    pub fn new(risk: ApprovalRisk) -> Self {
        Self { risk, money: false, abilities: Vec::new() }
    }

    /// Marks it as spending money; the risk is then at least high.
    #[must_use]
    pub fn with_money(mut self) -> Self {
        self.money = true;
        self.risk = self.risk.max(ApprovalRisk::High);
        self
    }

    /// Adds an ability.
    #[must_use]
    pub fn with_ability(mut self, ability: impl Into<String>) -> Self {
        self.abilities.push(ability.into());
        self
    }

    /// The higher risk of the two, money if either spends it, and the abilities of both,
    /// each once, `self`'s first.
    #[must_use]
    pub fn merge(mut self, other: Self) -> Self {
        self.risk = self.risk.max(other.risk);
        self.money |= other.money;
        for ability in other.abilities {
            if !self.abilities.contains(&ability) {
                self.abilities.push(ability);
            }
        }
        if self.money {
            self.risk = self.risk.max(ApprovalRisk::High);
        }
        self
    }
}

/// The most a suggesting agent may give an agent it suggests: its own limits now (spec 3.3
/// §5.7, S-I12).
///
/// # Example
///
/// ```rust
/// use adk_kernel::agents::{AgentCeiling, AgentUses, FileAccess};
/// use adk_kernel::PartitionMode;
///
/// let ceiling = AgentCeiling::new(PartitionMode::Ask, AgentUses::default().with_files(FileAccess::Read))
///     .with_daily_limit(2_000_000);
/// assert_eq!(ceiling.mode, PartitionMode::Ask);
/// assert_eq!(ceiling.daily_limit_micro_usd, Some(2_000_000));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AgentCeiling {
    /// The suggesting agent's mode in the suggesting session.
    pub mode: PartitionMode,
    /// What the suggesting agent can use.
    pub uses: AgentUses,
    /// What the suggesting agent may spend a day, in micro-USD; `None` when nothing limits
    /// it (spec 3.3 S-I12). The kernel lowers it to the area's daily limit, which every agent
    /// in the area spends within.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_limit_micro_usd: Option<u64>,
}

impl AgentCeiling {
    /// A ceiling of `mode` and `uses`, with no spending limit of its own.
    pub fn new(mode: PartitionMode, uses: AgentUses) -> Self {
        Self { mode, uses, daily_limit_micro_usd: None }
    }

    /// Sets what the suggesting agent may spend a day, in micro-USD (spec 3.3 S-I12).
    #[must_use]
    pub fn with_daily_limit(mut self, micro_usd: u64) -> Self {
        self.daily_limit_micro_usd = Some(micro_usd);
        self
    }

    /// This ceiling with its spending limit lowered to `cap`, when there is one.
    #[must_use]
    pub(crate) fn within_daily_limit(mut self, cap: Option<u64>) -> Self {
        self.daily_limit_micro_usd = match (self.daily_limit_micro_usd, cap) {
            (Some(own), Some(cap)) => Some(own.min(cap)),
            (own, cap) => own.or(cap),
        };
        self
    }
}

/// What a person may choose for an agent on this host (spec 3.3 §5.7, decision 21): the
/// models of every provider whose key the host has, and the packs it can give.
///
/// # Example
///
/// ```rust
/// use adk_kernel::agents::{AgentChoices, ModelChoice};
///
/// let choices = AgentChoices::default().with_model(ModelChoice::new("anthropic", "claude-sonnet-5-5"));
/// assert_eq!(choices.models.len(), 1);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AgentChoices {
    /// The models an agent may run on, the primary agent's first.
    #[serde(default)]
    pub models: Vec<ModelChoice>,
    /// The packs an agent may have.
    #[serde(default)]
    pub packs: Vec<String>,
}

impl AgentChoices {
    /// Adds a model.
    #[must_use]
    pub fn with_model(mut self, model: ModelChoice) -> Self {
        self.models.push(model);
        self
    }

    /// Adds a pack.
    #[must_use]
    pub fn with_pack(mut self, pack: impl Into<String>) -> Self {
        self.packs.push(pack.into());
        self
    }
}

/// What a hosted agent holds beyond its runner, closed once the kernel stops serving that
/// build of the agent: when a change replaced it, the agent was removed, or the kernel stops
/// (spec 3.3 §5.7). A harness closes its toolsets, its container shell's sessions and
/// distils its sessions' turns into long-term memory.
///
/// # Example
///
/// ```rust
/// use adk_kernel::agents::HostedClose;
///
/// struct Quiet;
///
/// #[async_trait::async_trait]
/// impl HostedClose for Quiet {
///     async fn close(&self) {}
/// }
/// ```
#[async_trait]
pub trait HostedClose: Send + Sync {
    /// Closes it. Called once, after the runs that started on it have finished.
    async fn close(&self);
}

/// An agent a factory built: its runner, and the progress source its harness reports usage,
/// goal progress and loop-guard verdicts through (spec 3.3 §5.7). The kernel attaches the
/// source when it hosts the agent, so the agent's spending reaches its own processes.
#[derive(Clone)]
#[non_exhaustive]
pub struct HostedAgent {
    /// The runner, whose root agent is named by the record's id.
    pub runner: Arc<Runner>,
    /// The agent's progress source, when its harness keeps one.
    pub progress: Option<Arc<dyn ProgressSource>>,
    /// What closes it once the kernel stops serving it; without one the kernel closes the
    /// runner.
    pub close: Option<Arc<dyn HostedClose>>,
}

impl HostedAgent {
    /// An agent built as `runner`, with no progress source.
    pub fn new(runner: Arc<Runner>) -> Self {
        Self { runner, progress: None, close: None }
    }

    /// Sets the progress source.
    #[must_use]
    pub fn with_progress(mut self, progress: Arc<dyn ProgressSource>) -> Self {
        self.progress = Some(progress);
        self
    }

    /// Sets what closes the agent once the kernel stops serving it, in place of closing its
    /// runner.
    #[must_use]
    pub fn with_close(mut self, close: Arc<dyn HostedClose>) -> Self {
        self.close = Some(close);
        self
    }
}

impl std::fmt::Debug for HostedAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostedAgent")
            .field("root", &self.runner.root_agent_name())
            .field("progress", &self.progress.is_some())
            .field("close", &self.close.is_some())
            .finish()
    }
}

/// How a host builds the agents a person adds (spec 3.3 §5.7, §10 K7), registered with
/// `KernelBuilder::agent_factory` beside the agent store. `adk-rust up`'s factory builds a
/// harness agent with the composition the primary agent uses.
///
/// The kernel adds its own checks on top of [`assess`](Self::assess) and
/// [`ceiling`](Self::ceiling): the risk of the agent's drivers by their effect class, its
/// mode at most its area's, its limit at most its area's; the higher risk wins.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::{AgentId, ApprovalRisk};
/// use adk_kernel::agents::{
///     AgentAssessment, AgentCeiling, AgentChoices, AgentFactory, AgentRecord, HostedAgent,
/// };
/// use async_trait::async_trait;
///
/// struct Fixed(Arc<adk_runner::Runner>);
///
/// #[async_trait]
/// impl AgentFactory for Fixed {
///     fn assess(&self, _record: &AgentRecord) -> AgentAssessment {
///         AgentAssessment::new(ApprovalRisk::Low)
///     }
///     async fn ceiling(&self, _agent: &AgentId, _session: &str) -> adk_kernel::Result<Option<AgentCeiling>> {
///         Ok(None)
///     }
///     async fn choices(&self) -> adk_kernel::Result<AgentChoices> {
///         Ok(AgentChoices::default())
///     }
///     async fn build(&self, _record: &AgentRecord) -> adk_kernel::Result<HostedAgent> {
///         Ok(HostedAgent::new(self.0.clone()))
///     }
///     async fn retired(&self, _record: &AgentRecord) -> adk_kernel::Result<u64> {
///         Ok(0)
///     }
/// }
/// ```
#[async_trait]
pub trait AgentFactory: Send + Sync {
    /// The risk, money and abilities of `record`'s definition (spec 3.3 §5.7's risk table).
    fn assess(&self, record: &AgentRecord) -> AgentAssessment;

    /// The suggesting agent `agent`'s limits now, in `session`, the session it suggested
    /// in; `None` when it may suggest nothing.
    ///
    /// # Errors
    ///
    /// Returns the host's error; the kernel then refuses the suggestion.
    async fn ceiling(&self, agent: &AgentId, session: &str) -> Result<Option<AgentCeiling>>;

    /// The suggesting agent `agent`'s limits now in `session`, as the agent of `area`, the
    /// area it suggests in: a host that runs one agent per area answers with that area's
    /// limits, its drivers included (spec 3.3 S-I12). By default [`ceiling`](Self::ceiling).
    ///
    /// # Errors
    ///
    /// Returns the host's error; the kernel then refuses the suggestion.
    async fn ceiling_in(
        &self,
        agent: &AgentId,
        session: &str,
        area: &PartitionId,
    ) -> Result<Option<AgentCeiling>> {
        let _ = area;
        self.ceiling(agent, session).await
    }

    /// `definition` as the kernel keeps an agent a person sets up by hand: its instructions
    /// with secrets redacted and injection patterns labelled, as fetched text is, and its
    /// line with secrets redacted (spec 3.3 §5.7, spec 3.1 I47). Screening twice changes
    /// nothing. By default as it is.
    async fn screen(&self, definition: AgentDefinition) -> AgentDefinition {
        definition
    }

    /// What a person may choose for an agent: the models whose key the host has, and the
    /// packs.
    ///
    /// # Errors
    ///
    /// Returns the host's error.
    async fn choices(&self) -> Result<AgentChoices>;

    /// Builds `record`, an active or paused agent, with its root agent named by its id.
    ///
    /// # Errors
    ///
    /// Returns why it cannot be built, such as a missing key; the kernel keeps the record
    /// and shows the agent as "Can't start" with the reason.
    async fn build(&self, record: &AgentRecord) -> Result<HostedAgent>;

    /// Called once when `record` is removed: revokes the standing rules pinned to it and
    /// answers how many it revoked.
    ///
    /// # Errors
    ///
    /// Returns the host's error; the next tick calls it again.
    async fn retired(&self, record: &AgentRecord) -> Result<u64>;

    /// Ids no added agent may take beside the kernel's own: member names, say.
    fn reserved_ids(&self) -> Vec<String> {
        Vec::new()
    }

    /// Names no added agent may take, ignoring case: the primary agent's display name.
    fn reserved_names(&self) -> Vec<String> {
        Vec::new()
    }
}
