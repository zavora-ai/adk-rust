//! The site's jobs and the kernel processes that run them (spec 3.5 §7.20, §7.11, §7.19).
//!
//! Every job of the §7.20 table is a [`JobSpec`]: its area, its agents, the level it starts
//! at, its effects, the back-office tools that are its, its playbook and, for the jobs the
//! kernel wakes, its site process kind. [`bindings`] maps the jobs a kind runs onto
//! `adk-harness`' [`JobBinding`]s for the ladder; [`register`] brings a signed charter's
//! authority onto the kernel's ladder with each job's area, so an incident demotes the jobs
//! of its area (§7.11).
//!
//! | Process (§7.11) | Job | Agent | Default wake |
//! |---|---|---|---|
//! | `site.probe` | `protect` | Guardian | every 5 minutes |
//! | `site.watch` | `hosting` | Guardian | daily at 06:00 UTC |
//! | `site.content` | `freshness` | Growth | a `content_gap` signal |
//! | `site.reviews` | `social` (playbook `review-replies`) | Growth | a `review_seen` signal |
//! | `site.outreach` | `nurture` | Revenue | daily at 09:00 UTC |
//! | `site.support` | `owner-questions` | Steward | a `support_escalation` signal |
//! | `site.digest` | `weekly-review` | Steward | Mondays at 07:00 UTC |

use std::fmt;

use adk_core::{NewRun, Result};
use adk_harness::website::{JobBinding, WebsitePartition};
use adk_kernel::PartitionId;
use adk_kernel::charter::{AutonomyLadder, JobAutonomy, SiteProcessKind, ladder_rank};
use adk_kernel::wake::Trigger;
use awp_types::{AutonomyLevel, Charter, JobAuthority, JobLimits};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::team::{AgentRole, SiteTeam};

use AgentRole::{AgentDesk, Concierge, Finance, Growth, Guardian, Inbox, Revenue, Steward};
use AutonomyLevel::{ActReversibleOnly, ActWithApproval, ActWithinBudget, Propose};
use JobEffect::{
    CapabilityEffects, Financial, OwnerChannels, PublishPublic, Read, SendExternal, Signals,
};

/// The areas of the §7.20 table; an incident in an area demotes its jobs (spec 3.5 §7.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobArea {
    /// Run and protect.
    RunAndProtect,
    /// Be found.
    BeFound,
    /// Serve people and agents.
    Serve,
    /// Grow: enquiries, orders, donations, subscribers or service completions by kind.
    Grow,
    /// Money.
    Money,
    /// Account to the owner.
    Account,
}

impl JobArea {
    /// The area's name, as `JobBinding::in_area` and the ladder record it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RunAndProtect => "run-and-protect",
            Self::BeFound => "be-found",
            Self::Serve => "serve",
            Self::Grow => "grow",
            Self::Money => "money",
            Self::Account => "account",
        }
    }
}

impl fmt::Display for JobArea {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A job's effects (spec 3.5 §7.20 "Effects").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobEffect {
    /// Reads only.
    Read,
    /// Publishes a signed release.
    PublishPublic,
    /// Moves money.
    Financial,
    /// Sends to someone outside.
    SendExternal,
    /// Reaches the owner on their channels.
    OwnerChannels,
    /// Files typed signals.
    Signals,
    /// The declared capabilities' effects.
    CapabilityEffects,
}

/// Where a job starts (spec 3.5 §7.20 "Starts at").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "policy", content = "level", rename_all = "snake_case")]
pub enum StartsAt {
    /// A public job: the capability manifest's policy decides, not the ladder.
    Manifest,
    /// A back-office job: this autonomy level on the ladder.
    Level(AutonomyLevel),
}

/// What a kind must allow for a job to exist (spec 3.5 §7.16.1, G13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobNeed {
    /// Nothing beyond its agents.
    Nothing,
    /// Advertising.
    Advertising,
    /// Sales: quotes, checkout, orders, donations or subscriptions.
    Sales,
    /// The wallet beyond hosting.
    Wallet,
}

/// One job of the §7.20 table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobSpec {
    /// The job's id, as the charter's authority names it.
    pub id: &'static str,
    /// Its area.
    pub area: JobArea,
    /// What it is.
    pub title: &'static str,
    /// Its agents: the first back-office one holds its ladder level.
    pub agents: &'static [AgentRole],
    /// Where it starts.
    pub starts_at: StartsAt,
    /// The table's qualifier, such as "Propose for new pages".
    pub note: &'static str,
    /// Its effects.
    pub effects: &'static [JobEffect],
    /// Globs over the back-office tools that are its.
    pub tools: &'static [&'static str],
    /// What the kind must allow.
    pub needs: JobNeed,
    /// The site process the kernel runs it as.
    pub process: Option<SiteProcessKind>,
    /// Its playbook.
    pub playbook: &'static str,
}

impl JobSpec {
    /// The back-office agent whose level the job's ladder record holds, among `team`'s
    /// members; `None` for a public job or a job none of whose agents runs.
    pub fn ladder_agent(&self, team: &SiteTeam) -> Option<AgentRole> {
        match self.starts_at {
            StartsAt::Manifest => None,
            StartsAt::Level(_) => {
                self.agents.iter().copied().find(|role| {
                    role.partition() == WebsitePartition::BackOffice && team.runs(*role)
                })
            }
        }
    }

    /// The level charter version 1 gives the job (spec 3.5 §7.18 "Rules", stage 9): every
    /// job starts at Observe or Propose for the first week; onboarding itself acts only with
    /// the owner's approval of each change. `None` for a public job.
    pub fn first_week_level(&self) -> Option<AutonomyLevel> {
        let StartsAt::Level(level) = self.starts_at else { return None };
        Some(match (self.id, level) {
            (ONBOARDING, _) => AutonomyLevel::ActWithApproval,
            (_, AutonomyLevel::Observe) => AutonomyLevel::Observe,
            _ => AutonomyLevel::Propose,
        })
    }
}

/// The onboarding job: the Steward builds and launches release 1 (spec 3.5 §7.18).
pub const ONBOARDING: &str = "onboarding";

/// Every job: the §7.20 table and the onboarding job.
///
/// # Example
///
/// ```rust
/// use adk_website::jobs::{JOBS, job};
///
/// assert_eq!(JOBS.len(), 25);
/// assert_eq!(job("protect").unwrap().playbook, "protect");
/// ```
pub const JOBS: &[JobSpec] = &[
    JobSpec {
        id: ONBOARDING,
        area: JobArea::Account,
        title: "Onboarding and launch: build release 1 while the owner talks",
        agents: &[Steward],
        starts_at: StartsAt::Level(ActWithApproval),
        note: "Production publication at launch is held in every mode (A-I21)",
        effects: &[PublishPublic],
        tools: &[
            "content_*",
            "translation_draft",
            "media_intake",
            "release_preview",
            "site_publish",
            "site_discover",
            "interview_record",
            "launch_gate_run",
            "awp_verify",
            "host_fetch",
        ],
        needs: JobNeed::Nothing,
        process: None,
        playbook: "onboarding",
    },
    JobSpec {
        id: "protect",
        area: JobArea::RunAndProtect,
        title: "Static content, signed releases, drift rollback, site watch, abuse throttling",
        agents: &[Guardian],
        starts_at: StartsAt::Level(ActReversibleOnly),
        note: "Rollback and read-only are the only incident acts (A-I11)",
        effects: &[PublishPublic],
        tools: &["site_rollback", "edge_read_only", "host_fetch", "awp_verify"],
        needs: JobNeed::Nothing,
        process: Some(SiteProcessKind::Probe),
        playbook: "protect",
    },
    JobSpec {
        id: "alarms",
        area: JobArea::RunAndProtect,
        title: "Alarms by urgency and channel",
        agents: &[Guardian],
        starts_at: StartsAt::Level(ActWithinBudget),
        note: "Act",
        effects: &[OwnerChannels],
        tools: &[],
        needs: JobNeed::Nothing,
        process: None,
        playbook: "alarms",
    },
    JobSpec {
        id: "freshness",
        area: JobArea::RunAndProtect,
        title: "Freshness: stale facts, broken links, outdated pages",
        agents: &[Growth],
        starts_at: StartsAt::Level(ActWithinBudget),
        note: "Within budget for edits; Propose for new pages",
        effects: &[PublishPublic],
        tools: &[
            "content_draft",
            "content_edit",
            "content_correct",
            "content_revert",
            "content_schedule",
            "release_preview",
            "site_publish",
        ],
        needs: JobNeed::Nothing,
        process: Some(SiteProcessKind::Content),
        playbook: "freshness",
    },
    JobSpec {
        id: "hosting",
        area: JobArea::RunAndProtect,
        title: "Hosting, domains, certificates, backups",
        agents: &[Guardian, Finance],
        starts_at: StartsAt::Level(Propose),
        note: "Propose; renewals within budget",
        effects: &[PublishPublic, Financial],
        tools: &["launch_gate_run"],
        needs: JobNeed::Nothing,
        process: Some(SiteProcessKind::Watch),
        playbook: "hosting",
    },
    JobSpec {
        id: "seo",
        area: JobArea::BeFound,
        title: "SEO",
        agents: &[Growth],
        starts_at: StartsAt::Level(ActWithinBudget),
        note: "Within budget",
        effects: &[PublishPublic],
        tools: &["content_edit", "release_preview", "site_publish"],
        needs: JobNeed::Nothing,
        process: None,
        playbook: "seo",
    },
    JobSpec {
        id: "aeo",
        area: JobArea::BeFound,
        title: "AEO",
        agents: &[Growth],
        starts_at: StartsAt::Level(ActWithinBudget),
        note: "Within budget; tracking at Observe",
        effects: &[PublishPublic, Read],
        tools: &["content_draft", "content_edit", "release_preview", "site_publish"],
        needs: JobNeed::Nothing,
        process: None,
        playbook: "aeo",
    },
    JobSpec {
        id: "agent-search",
        area: JobArea::BeFound,
        title: "Agent search",
        agents: &[Growth],
        starts_at: StartsAt::Level(ActWithinBudget),
        note: "Act",
        effects: &[PublishPublic],
        tools: &["release_preview", "site_publish", "awp_verify"],
        needs: JobNeed::Nothing,
        process: None,
        playbook: "agent-search",
    },
    JobSpec {
        id: "multilingual",
        area: JobArea::Serve,
        title: "Multilingual pages and conversation",
        agents: &[Concierge, Growth],
        starts_at: StartsAt::Level(ActWithinBudget),
        note: "Act; a new language at Propose",
        effects: &[PublishPublic],
        tools: &["translation_draft", "release_preview", "site_publish"],
        needs: JobNeed::Nothing,
        process: None,
        playbook: "multilingual",
    },
    JobSpec {
        id: "presentations",
        area: JobArea::Serve,
        title: "Presentations by role and need",
        agents: &[Concierge],
        starts_at: StartsAt::Manifest,
        note: "Act",
        effects: &[],
        tools: &[],
        needs: JobNeed::Nothing,
        process: None,
        playbook: "presentations",
    },
    JobSpec {
        id: "agent-front-door",
        area: JobArea::Serve,
        title: "Agent front door",
        agents: &[AgentDesk],
        starts_at: StartsAt::Manifest,
        note: "Act within policy",
        effects: &[CapabilityEffects],
        tools: &[],
        needs: JobNeed::Nothing,
        process: None,
        playbook: "agent-front-door",
    },
    JobSpec {
        id: "email",
        area: JobArea::Serve,
        title: "The site's email",
        agents: &[Inbox, Revenue],
        starts_at: StartsAt::Level(ActWithinBudget),
        note: "Within playbook; Propose otherwise",
        effects: &[SendExternal],
        tools: &[],
        needs: JobNeed::Nothing,
        process: None,
        playbook: "email",
    },
    JobSpec {
        id: "social",
        area: JobArea::Serve,
        title: "Social engagement",
        agents: &[Inbox, Growth],
        starts_at: StartsAt::Level(Propose),
        note: "Propose, then playbook",
        effects: &[SendExternal, PublishPublic],
        tools: &["content_draft", "content_edit", "release_preview", "site_publish", "social_post"],
        needs: JobNeed::Nothing,
        process: Some(SiteProcessKind::Reviews),
        playbook: "review-replies",
    },
    JobSpec {
        id: "advertising",
        area: JobArea::Serve,
        title: "Advertising",
        agents: &[Growth, Finance],
        starts_at: StartsAt::Level(Propose),
        note: "Propose to launch; within budget to tune",
        effects: &[Financial, PublishPublic],
        tools: &[],
        needs: JobNeed::Advertising,
        process: None,
        playbook: "advertising",
    },
    JobSpec {
        id: "capture",
        area: JobArea::Grow,
        title: "Lead, sign-up or request capture from every channel",
        agents: &[Concierge, Inbox, AgentDesk],
        starts_at: StartsAt::Manifest,
        note: "Act",
        effects: &[Signals],
        tools: &[],
        needs: JobNeed::Nothing,
        process: None,
        playbook: "capture",
    },
    JobSpec {
        id: "nurture",
        area: JobArea::Grow,
        title: "Qualify and nurture with consent",
        agents: &[Revenue],
        starts_at: StartsAt::Level(ActWithinBudget),
        note: "Within playbook",
        effects: &[SendExternal],
        tools: &["email_send_sequence_step"],
        needs: JobNeed::Nothing,
        process: Some(SiteProcessKind::Outreach),
        playbook: "nurture",
    },
    JobSpec {
        id: "handoff",
        area: JobArea::Grow,
        title: "Warm handoff with context",
        agents: &[Revenue],
        starts_at: StartsAt::Level(ActWithinBudget),
        note: "Act",
        effects: &[OwnerChannels],
        tools: &["lead_handoff"],
        needs: JobNeed::Nothing,
        process: None,
        playbook: "handoff",
    },
    JobSpec {
        id: "sell",
        area: JobArea::Grow,
        title: "Sell: quotes, checkout, bookings, receipts",
        agents: &[Revenue],
        starts_at: StartsAt::Level(ActWithinBudget),
        note: "Within payment policy",
        effects: &[Financial],
        tools: &[],
        needs: JobNeed::Sales,
        process: None,
        playbook: "sell",
    },
    JobSpec {
        id: "experiments",
        area: JobArea::Grow,
        title: "Experiments",
        agents: &[Growth],
        starts_at: StartsAt::Level(ActWithinBudget),
        note: "Within budget",
        effects: &[PublishPublic],
        tools: &[
            "content_draft",
            "release_preview",
            "site_publish",
            "experiment_start",
            "experiment_report",
        ],
        needs: JobNeed::Nothing,
        process: None,
        playbook: "experiments",
    },
    JobSpec {
        id: "playbook-learning",
        area: JobArea::Grow,
        title: "Playbook learning",
        agents: &[Steward],
        starts_at: StartsAt::Level(Propose),
        note: "Propose",
        effects: &[],
        tools: &[],
        needs: JobNeed::Nothing,
        process: None,
        playbook: "playbook-learning",
    },
    JobSpec {
        id: "running-costs",
        area: JobArea::Money,
        title: "Pay running costs under mandates",
        agents: &[Finance],
        starts_at: StartsAt::Level(ActWithinBudget),
        note: "Within budget",
        effects: &[Financial],
        tools: &["wallet_pay"],
        needs: JobNeed::Wallet,
        process: None,
        playbook: "running-costs",
    },
    JobSpec {
        id: "cost-and-return",
        area: JobArea::Money,
        title: "Cost and return; budget moves within caps",
        agents: &[Finance],
        starts_at: StartsAt::Level(ActWithinBudget),
        note: "Within budget",
        effects: &[],
        tools: &["wallet_status"],
        needs: JobNeed::Wallet,
        process: None,
        playbook: "cost-and-return",
    },
    JobSpec {
        id: "goals",
        area: JobArea::Account,
        title: "Goals, strategy, KPIs",
        agents: &[Steward],
        starts_at: StartsAt::Level(Propose),
        note: "Propose",
        effects: &[],
        tools: &["charter_draft", "charter_commit_request"],
        needs: JobNeed::Nothing,
        process: None,
        playbook: "goals",
    },
    JobSpec {
        id: "weekly-review",
        area: JobArea::Account,
        title: "Weekly review and reports",
        agents: &[Steward, Finance],
        starts_at: StartsAt::Level(ActWithinBudget),
        note: "Act",
        effects: &[OwnerChannels],
        tools: &["job_promotion_request"],
        needs: JobNeed::Nothing,
        process: Some(SiteProcessKind::Digest),
        playbook: "weekly-review",
    },
    JobSpec {
        id: "owner-questions",
        area: JobArea::Account,
        title: "Questions for the owner",
        agents: &[Steward],
        starts_at: StartsAt::Level(ActWithinBudget),
        note: "Act",
        effects: &[OwnerChannels],
        tools: &[],
        needs: JobNeed::Nothing,
        process: Some(SiteProcessKind::Support),
        playbook: "owner-questions",
    },
];

/// The job `id`.
pub fn job(id: &str) -> Option<&'static JobSpec> {
    JOBS.iter().find(|job| job.id == id)
}

/// The ladder bindings of `jobs` for `team` (spec 3.5 §7.11): each back-office job with an
/// agent that runs, its tools and its area. Bind them with `LadderCompiler::with_charter`.
///
/// # Example
///
/// ```rust
/// use adk_harness::website::SiteRef;
/// use adk_website::{jobs, kinds::template, team::SiteTeam};
/// use awp_types::SiteKind;
///
/// let site = SiteRef::new(uuid::Uuid::nil(), "notes", "https://notes.example");
/// let personal = template(SiteKind::Personal);
/// let team = SiteTeam::new(site, &personal);
/// let bindings = jobs::bindings(&team, &personal.jobs());
/// assert!(bindings.iter().any(|b| b.job == "protect" && b.agent == "guardian"));
/// assert!(bindings.iter().all(|b| b.agent != "revenue"), "a personal site runs no Revenue");
/// ```
pub fn bindings(team: &SiteTeam, jobs: &[&JobSpec]) -> Vec<JobBinding> {
    jobs.iter()
        .filter_map(|job| {
            let agent = job.ladder_agent(team)?;
            Some(
                JobBinding::new(job.id, agent.as_str(), job.tools.iter().copied())
                    .in_area(job.area.as_str()),
            )
        })
        .collect()
}

/// The authority charter version 1 gives `jobs` (spec 3.5 §7.18 stage 5 "conservative
/// authority", "Rules"): each back-office job at its first-week level, with
/// `actions_per_day` for jobs that will act within budget.
pub fn first_week_authority(
    team: &SiteTeam,
    jobs: &[&JobSpec],
    actions_per_day: u32,
) -> Vec<JobAuthority> {
    jobs.iter()
        .filter(|job| job.ladder_agent(team).is_some())
        .filter_map(|job| {
            let level = job.first_week_level()?;
            let limits = match job.starts_at {
                StartsAt::Level(ActWithinBudget) => {
                    JobLimits { spend_cap: None, actions_per_day: Some(actions_per_day) }
                }
                _ => JobLimits::default(),
            };
            Some(JobAuthority { job: job.id.to_string(), level, limits })
        })
        .collect()
}

/// The promotions the 30/60/90-day plan proposes after the first week (spec 3.5 §7.18 stage
/// 9): each job below its §7.20 starting level, with that level. Each is offered to the
/// owner; only the owner's decision raises a level (A-I13).
pub fn starting_levels(charter: &Charter) -> Vec<(String, AutonomyLevel)> {
    charter
        .authority
        .iter()
        .filter_map(|authority| {
            let StartsAt::Level(start) = job(&authority.job)?.starts_at else { return None };
            (ladder_rank(start) > ladder_rank(authority.level))
                .then(|| (authority.job.clone(), start))
        })
        .collect()
}

/// Brings a committed charter's authority onto the kernel's ladder (spec 3.5 §7.11): each
/// job registered with its area first, then `sync`, which lowers a job above the charter's
/// level and never raises one.
///
/// # Errors
///
/// Returns the autonomy store's error.
pub async fn register(ladder: &AutonomyLadder, charter: &Charter) -> Result<Vec<JobAutonomy>> {
    let error = |e: adk_kernel::KernelError| adk_core::AdkError::config(e.to_string());
    for authority in &charter.authority {
        let area = job(&authority.job).map(|job| job.area.as_str());
        ladder
            .register(charter.site_id, &authority.job, authority.level, area)
            .await
            .map_err(error)?;
    }
    ladder.sync(charter).await.map_err(error)
}

/// Registers the onboarding job of `site` before charter version 1 exists (spec 3.5 §7.18):
/// every change onboarding makes acts only with the owner's approval, and the launch is held
/// in every mode (A-I21). Charter version 1 keeps it there.
///
/// # Errors
///
/// Returns the autonomy store's error.
pub async fn register_onboarding(ladder: &AutonomyLadder, site: uuid::Uuid) -> Result<JobAutonomy> {
    ladder
        .register(site, ONBOARDING, AutonomyLevel::ActWithApproval, Some(JobArea::Account.as_str()))
        .await
        .map_err(|e| adk_core::AdkError::config(e.to_string()))
}

/// A site process: what the kernel starts for a job, on which wake, as which run (spec 3.5
/// §7.11, §7.19, §7.20).
#[derive(Debug, Clone, PartialEq)]
pub struct SiteProcess {
    /// The process kind.
    pub kind: SiteProcessKind,
    /// The job.
    pub job: &'static str,
    /// The agent that does the work, or the workflow that runs it.
    pub target: String,
    /// The standing trigger.
    pub trigger: Trigger,
}

/// The workflow the Guardian's probe runs without a model: probe, drift, incident path
/// (spec 3.5 §7.20).
pub const PROBE_WORKFLOW: &str = "website.guardian.probe";
/// The workflow that composes and sends the weekly review (spec 3.5 §7.21).
pub const DIGEST_WORKFLOW: &str = "website.steward.digest";

/// The processes of `jobs` for `team`, in the back office `partition` of app `app` (spec 3.5
/// §7.11, §7.20): the probe and the digest run as workflows; the other kinds run their job's
/// agent with the playbook as its turn.
///
/// # Example
///
/// ```rust
/// use adk_harness::website::SiteRef;
/// use adk_website::{jobs, kinds::template, team::SiteTeam};
/// use adk_kernel::charter::SiteProcessKind;
/// use awp_types::SiteKind;
///
/// let site = SiteRef::new(uuid::Uuid::nil(), "notes", "https://notes.example");
/// let personal = template(SiteKind::Personal);
/// let team = SiteTeam::new(site, &personal);
/// let processes = jobs::processes(&team, &personal.jobs(), "notes", &"notes.back-office".into());
/// let kinds: Vec<SiteProcessKind> = processes.iter().map(|p| p.kind).collect();
/// assert!(kinds.contains(&SiteProcessKind::Probe));
/// assert!(!kinds.contains(&SiteProcessKind::Outreach), "no Revenue on a personal site");
/// ```
pub fn processes(
    team: &SiteTeam,
    jobs: &[&JobSpec],
    app: &str,
    partition: &PartitionId,
) -> Vec<SiteProcess> {
    jobs.iter()
        .filter_map(|job| {
            let kind = job.process?;
            let agent = job.ladder_agent(team)?;
            let target = match kind {
                SiteProcessKind::Probe => PROBE_WORKFLOW.to_string(),
                SiteProcessKind::Digest => DIGEST_WORKFLOW.to_string(),
                _ => agent.as_str().to_string(),
            };
            let input = json!({
                "site": team.site().id,
                "job": job.id,
                "playbook": job.playbook,
                "text": format!("Run the {} playbook for job {}.", job.playbook, job.id),
            });
            // `trigger` puts the run on the kind's queue.
            let run = NewRun::new(
                kind.queue().as_str(),
                app,
                format!("site:{}", team.site().name),
                &target,
                input,
            );
            let trigger = kind.trigger(kind.default_wake(partition), run, partition.clone());
            Some(SiteProcess { kind, job: job.id, target, trigger })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_every_job_has_its_playbook() {
        let mut ids: Vec<&str> = JOBS.iter().map(|j| j.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), JOBS.len());
        for job in JOBS {
            assert!(crate::playbooks::playbook(job.playbook).is_some(), "{}", job.id);
        }
    }

    /// Every back-office job binds the tools it acts with, and each is one its agent holds
    /// (spec 3.5 §7.19, §7.20): a job with none could be promoted without any standing rule.
    #[test]
    fn every_acting_job_binds_tools_its_agent_holds() {
        let unbound: Vec<&str> = JOBS
            .iter()
            .filter(|job| matches!(job.starts_at, StartsAt::Level(_)) && job.tools.is_empty())
            .map(|job| job.id)
            .collect();
        // Jobs whose work is the kernel's (alarms, the review's questions), drafting prose
        // with no tool of their own (playbook learning), or a driver the product does not
        // mount yet (email, advertising, sales).
        assert_eq!(
            unbound,
            ["alarms", "email", "advertising", "sell", "playbook-learning", "owner-questions"]
        );
        for job in JOBS.iter().filter(|job| matches!(job.starts_at, StartsAt::Level(_))) {
            let agent = job
                .agents
                .iter()
                .find(|role| role.partition() == WebsitePartition::BackOffice)
                .expect("a back-office job has a back-office agent");
            let held = JobBinding::new(job.id, agent.as_str(), agent.tools().iter().copied());
            for tool in job.tools {
                assert!(held.covers(tool), "{} binds {tool}, which {agent} does not hold", job.id);
            }
        }
    }

    #[test]
    fn first_week_levels_are_observe_or_propose_but_onboarding() {
        for job in JOBS {
            match job.first_week_level() {
                None => assert_eq!(job.starts_at, StartsAt::Manifest),
                Some(AutonomyLevel::ActWithApproval) => assert_eq!(job.id, ONBOARDING),
                Some(level) => {
                    assert!(matches!(level, AutonomyLevel::Observe | AutonomyLevel::Propose))
                }
            }
        }
    }
}
