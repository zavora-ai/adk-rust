//! The back office the served kernel runs (spec 3.5 §7.19, §7.20, §7.11, §7.22, A-I13).
//!
//! With a model, [`serve`](fn@super::serve) builds the site's back-office agents — each a
//! `HarnessAgent` in the back-office partition with its jobs' tools — and hosts them on the
//! kernel: the Steward's target runs the whole back office as one team, delegating to the
//! others; every other agent also runs alone for its jobs' processes (`site.content`,
//! `site.reviews`, `site.outreach`, `site.support`).
//!
//! | Each agent gets | From |
//! |---|---|
//! | The ladder's mode and budget for its jobs | The kernel's autonomy store, read at every call |
//! | The owner's standing rules | `data/kernel.db`, where `adk-rust website jobs promote` grants them, so a promotion takes effect while the site serves |
//! | Held calls | The owner's approval inbox, the kernel's approval service |
//! | The publish delay | [`PublishDelay`](crate::publish_delay::PublishDelay) on the content-gap jobs |
//! | Revenue: nurture sends, the lead handoff | The site's email outbox through [`nurture_tools`](crate::leads::nurture_tools), [`handoff_tool_for`](crate::alarms::handoff_tool_for) |
//! | Growth: experiments | [`experiment_tools`](crate::experiments::experiment_tools) |
//! | Guardian: the edge's read-only switch | [`SiteEdge::read_only_tool`](crate::edge::SiteEdge::read_only_tool) |
//! | Finance: `wallet_pay`, `wallet_status` | The owner's wallet, [`SiteOptions::wallet`](super::SiteOptions::wallet) |

use std::sync::Arc;

use adk_core::{Llm, Result, Tool};
use adk_drivers::Side;
use adk_drivers::email::email_tools;
use adk_harness::InstructionsConfig;
use adk_harness::website::{BackOfficeKit, LadderCompiler, PublicKit, SiteSearch};
use adk_kernel::charter::CharterStore as _;
use adk_kernel::{KernelBuilder, PartitionId};

use super::kernel::KernelStores;
use super::site::RunningSite;
use crate::team::{AgentRole, BuiltTeam, SiteTeam, TeamKit};

impl RunningSite {
    /// The ladder compiler of the site's jobs: under the charter in force when there is one,
    /// with the email tools' origins pinned for standing rules.
    async fn compiler(&self, stores: &KernelStores) -> Result<LadderCompiler> {
        let team = SiteTeam::new(self.site.clone(), &self.template);
        let bindings = crate::jobs::bindings(&team, &self.template.jobs());
        let current = stores
            .charters
            .current(self.site.id)
            .await
            .map_err(|e| adk_core::AdkError::config(e.to_string()))?;
        let mut compiler = match current {
            Some(current) => {
                LadderCompiler::new(self.site.clone()).with_charter(&current.charter, bindings)
            }
            None => bindings
                .into_iter()
                .fold(LadderCompiler::new(self.site.clone()), LadderCompiler::job),
        };
        if let Some(outbox) = &self.outbox {
            compiler = compiler.with_tool_origins(&email_tools(outbox.clone(), Side::BackOffice));
        }
        Ok(compiler)
    }

    /// The site's back office over `model` (spec 3.5 §7.19): every back-office agent the
    /// kind runs, each with its jobs' tools, the owner's standing rules, the owner's approval
    /// inbox for held calls and the publish delay, compiled as one team led by the Steward.
    ///
    /// # Errors
    ///
    /// A store's error, or a team that does not build.
    pub async fn back_office(
        &self,
        stores: &KernelStores,
        model: Arc<dyn Llm>,
    ) -> Result<BuiltTeam> {
        let compiler = self.compiler(stores).await?;
        let mut kit = BackOfficeKit::new(
            self.partitions.clone(),
            stores.partitions.clone(),
            self.runtime.clone(),
            compiler.clone(),
            stores.autonomy.clone(),
        );
        // The KPI tracker's guardrails pause jobs through the same pauses (A-I15).
        kit.pauses = self.pauses.clone();
        let public = {
            let partitions = self.partitions.clone();
            let table = stores.partitions.clone();
            let signals = partitions.signal_sink(stores.signals.clone(), stores.agenda.clone());
            move || PublicKit {
                partitions: partitions.clone(),
                table: table.clone(),
                search: Arc::new(SiteSearch::new()),
                signals: signals.clone(),
                reply: None,
                capabilities: Vec::new(),
            }
        };
        let revenue: Vec<Arc<dyn Tool>> = {
            let mut tools =
                vec![crate::alarms::handoff_tool_for(self.alarms.clone(), self.leads.clone())];
            if let Some(outbox) = &self.outbox {
                tools.extend(crate::leads::nurture_tools(
                    email_tools(outbox.clone(), Side::BackOffice),
                    self.leads.clone(),
                    crate::alarms::quiet_hours(&self.reporting),
                    Arc::new(adk_core::SystemClock),
                ));
            }
            tools
        };
        let growth = crate::experiments::experiment_tools(self.experiments.clone());
        let guardian = vec![self.edge.read_only_tool()];
        let delay = crate::publish_delay::PublishDelay::new(self.publish_delay, compiler)
            .with_charters(stores.charters.clone());
        let delay: Arc<dyn adk_core::ToolPolicy> = Arc::new(delay);
        let approvals = stores.approvals.clone();
        let configure = move |role: AgentRole, builder: adk_harness::HarnessAgentBuilder| {
            let extra: &[Arc<dyn Tool>] = match role {
                AgentRole::Revenue => &revenue,
                AgentRole::Growth => &growth,
                AgentRole::Guardian => &guardian,
                _ => &[],
            };
            extra
                .iter()
                .cloned()
                .fold(builder, adk_harness::HarnessAgentBuilder::tool)
                // A served site reads no instruction file from the host's home directory.
                .project_instructions(InstructionsConfig {
                    user_file: None,
                    require_trust: false,
                    ..InstructionsConfig::default()
                })
                .approval_service(approvals.clone())
                .add_operator_policy(delay.clone())
        };
        // The packs' playbooks join each agent's own (spec 3.5 §7.23).
        self.packs.install_playbooks(&self.paths.data().join("agents")).await?;
        let kit = TeamKit {
            model,
            root: self.paths.data().join("agents"),
            back_office: kit,
            public: Arc::new(public),
            standing_rules: stores.rules.clone(),
            configure: Arc::new(configure),
            // The Finance agent pays running costs from the owner's wallet (spec 3.5 §7.14).
            wallet: self.wallet.clone(),
        };
        SiteTeam::new(self.site.clone(), &self.template).build(&kit).await
    }
}

/// Hosts `team` on `kernel` in the back-office `partition` of app `app` (spec 3.5 §7.19,
/// §7.20): the Steward's target runs the whole team; every other back-office agent runs alone
/// for its jobs' processes. Returns the agents hosted.
///
/// # Errors
///
/// A runner that refuses its configuration.
pub fn host(
    mut kernel: KernelBuilder,
    team: &BuiltTeam,
    partition: &PartitionId,
    app: &str,
    sessions: Arc<dyn adk_session::SessionService>,
) -> Result<(KernelBuilder, Vec<AgentRole>)> {
    let mut hosted = Vec::new();
    for role in team.roles() {
        if role.partition() != adk_harness::website::WebsitePartition::BackOffice {
            continue;
        }
        let runner = match role {
            AgentRole::Steward => team.team_runner(app, sessions.clone())?,
            _ => team.agent_runner(role, app, sessions.clone())?,
        };
        kernel = kernel.partition_agent(partition.clone(), role.as_str(), Arc::new(runner));
        hosted.push(role);
    }
    Ok((kernel, hosted))
}
