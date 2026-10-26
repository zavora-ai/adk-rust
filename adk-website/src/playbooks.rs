//! Playbooks: one skill file per job (spec 3.5 §7.16 `playbooks`, §7.15, §7.20, §7.23).
//!
//! Each job runs from a playbook, a Markdown skill file with YAML front matter
//! (`adk-skill`'s format). [`install`] writes an agent's playbooks into its workspace's
//! `.skills/`, where the harness discovers them. Outcomes feed proposals to revise a playbook,
//! which land only as the owner's decisions (§7.15); a developer's playbook replaces one by
//! id within the job's autonomy (§7.23).
//!
//! | Area | Playbooks |
//! |---|---|
//! | Run and protect | `protect`, `incident`, `alarms`, `freshness`, `hosting` |
//! | Be found | `seo`, `aeo`, `agent-search` |
//! | Serve | `multilingual`, `presentations`, `agent-front-door`, `email`, `social`, `review-replies`, `advertising` |
//! | Grow | `capture`, `nurture`, `handoff`, `sell`, `experiments`, `playbook-learning` |
//! | Money | `running-costs`, `cost-and-return` |
//! | Account | `onboarding`, `goals`, `weekly-review`, `owner-questions` |

use std::path::Path;

use adk_core::{AdkError, Result};

use crate::team::AgentRole;

/// A job's playbook (spec 3.5 §7.15, §7.20).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Playbook {
    /// The skill's name.
    pub id: &'static str,
    /// The job it runs (spec 3.5 §7.20).
    pub job: &'static str,
    /// The agents that use it.
    pub agents: &'static [AgentRole],
    /// The skill file: YAML front matter, then the body.
    pub text: &'static str,
}

/// Every playbook.
///
/// # Example
///
/// ```rust
/// use adk_website::playbooks::PLAYBOOKS;
///
/// assert!(PLAYBOOKS.iter().any(|p| p.id == "weekly-review"));
/// assert!(PLAYBOOKS.iter().all(|p| p.text.starts_with("---\nname: ")));
/// ```
pub const PLAYBOOKS: &[Playbook] = &[
    Playbook {
        id: "onboarding",
        job: "onboarding",
        agents: &[AgentRole::Steward],
        text: include_str!("../playbooks/onboarding.md"),
    },
    Playbook {
        id: "protect",
        job: "protect",
        agents: &[AgentRole::Guardian],
        text: include_str!("../playbooks/protect.md"),
    },
    Playbook {
        id: "incident",
        job: "protect",
        agents: &[AgentRole::Guardian],
        text: include_str!("../playbooks/incident.md"),
    },
    Playbook {
        id: "alarms",
        job: "alarms",
        agents: &[AgentRole::Guardian],
        text: include_str!("../playbooks/alarms.md"),
    },
    Playbook {
        id: "freshness",
        job: "freshness",
        agents: &[AgentRole::Growth],
        text: include_str!("../playbooks/freshness.md"),
    },
    Playbook {
        id: "hosting",
        job: "hosting",
        agents: &[AgentRole::Guardian, AgentRole::Finance],
        text: include_str!("../playbooks/hosting.md"),
    },
    Playbook {
        id: "seo",
        job: "seo",
        agents: &[AgentRole::Growth],
        text: include_str!("../playbooks/seo.md"),
    },
    Playbook {
        id: "aeo",
        job: "aeo",
        agents: &[AgentRole::Growth],
        text: include_str!("../playbooks/aeo.md"),
    },
    Playbook {
        id: "agent-search",
        job: "agent-search",
        agents: &[AgentRole::Growth],
        text: include_str!("../playbooks/agent-search.md"),
    },
    Playbook {
        id: "multilingual",
        job: "multilingual",
        agents: &[AgentRole::Concierge, AgentRole::Growth],
        text: include_str!("../playbooks/multilingual.md"),
    },
    Playbook {
        id: "presentations",
        job: "presentations",
        agents: &[AgentRole::Concierge],
        text: include_str!("../playbooks/presentations.md"),
    },
    Playbook {
        id: "agent-front-door",
        job: "agent-front-door",
        agents: &[AgentRole::AgentDesk],
        text: include_str!("../playbooks/agent-front-door.md"),
    },
    Playbook {
        id: "email",
        job: "email",
        agents: &[AgentRole::Inbox, AgentRole::Revenue],
        text: include_str!("../playbooks/email.md"),
    },
    Playbook {
        id: "social",
        job: "social",
        agents: &[AgentRole::Inbox, AgentRole::Growth],
        text: include_str!("../playbooks/social.md"),
    },
    Playbook {
        id: "review-replies",
        job: "social",
        agents: &[AgentRole::Growth, AgentRole::Inbox],
        text: include_str!("../playbooks/review-replies.md"),
    },
    Playbook {
        id: "advertising",
        job: "advertising",
        agents: &[AgentRole::Growth, AgentRole::Finance],
        text: include_str!("../playbooks/advertising.md"),
    },
    Playbook {
        id: "capture",
        job: "capture",
        agents: &[AgentRole::Concierge, AgentRole::Inbox, AgentRole::AgentDesk],
        text: include_str!("../playbooks/capture.md"),
    },
    Playbook {
        id: "nurture",
        job: "nurture",
        agents: &[AgentRole::Revenue],
        text: include_str!("../playbooks/nurture.md"),
    },
    Playbook {
        id: "handoff",
        job: "handoff",
        agents: &[AgentRole::Revenue],
        text: include_str!("../playbooks/handoff.md"),
    },
    Playbook {
        id: "sell",
        job: "sell",
        agents: &[AgentRole::Revenue],
        text: include_str!("../playbooks/sell.md"),
    },
    Playbook {
        id: "experiments",
        job: "experiments",
        agents: &[AgentRole::Growth],
        text: include_str!("../playbooks/experiments.md"),
    },
    Playbook {
        id: "playbook-learning",
        job: "playbook-learning",
        agents: &[AgentRole::Steward],
        text: include_str!("../playbooks/playbook-learning.md"),
    },
    Playbook {
        id: "running-costs",
        job: "running-costs",
        agents: &[AgentRole::Finance],
        text: include_str!("../playbooks/running-costs.md"),
    },
    Playbook {
        id: "cost-and-return",
        job: "cost-and-return",
        agents: &[AgentRole::Finance],
        text: include_str!("../playbooks/cost-and-return.md"),
    },
    Playbook {
        id: "goals",
        job: "goals",
        agents: &[AgentRole::Steward],
        text: include_str!("../playbooks/goals.md"),
    },
    Playbook {
        id: "weekly-review",
        job: "weekly-review",
        agents: &[AgentRole::Steward, AgentRole::Finance],
        text: include_str!("../playbooks/weekly-review.md"),
    },
    Playbook {
        id: "owner-questions",
        job: "owner-questions",
        agents: &[AgentRole::Steward],
        text: include_str!("../playbooks/owner-questions.md"),
    },
];

/// The playbook `id`.
pub fn playbook(id: &str) -> Option<&'static Playbook> {
    PLAYBOOKS.iter().find(|playbook| playbook.id == id)
}

/// The playbooks of `job`.
pub fn for_job(job: &str) -> impl Iterator<Item = &'static Playbook> + '_ {
    PLAYBOOKS.iter().filter(move |playbook| playbook.job == job)
}

/// Writes `role`'s playbooks into `workspace/.skills/{id}.md`, creating the directories; an
/// existing file with the same id is left as it is, so a developer's playbook wins (spec 3.5
/// §7.23).
///
/// # Errors
///
/// Returns the file system's error.
pub async fn install(role: AgentRole, workspace: &Path) -> Result<()> {
    let skills = workspace.join(".skills");
    tokio::fs::create_dir_all(&skills).await.map_err(|error| {
        AdkError::config(format!("cannot create {}: {error}", skills.display()))
    })?;
    for playbook in PLAYBOOKS.iter().filter(|playbook| playbook.agents.contains(&role)) {
        let path = skills.join(format!("{}.md", playbook.id));
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            continue;
        }
        tokio::fs::write(&path, playbook.text).await.map_err(|error| {
            AdkError::config(format!("cannot write {}: {error}", path.display()))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_every_agent_has_one() {
        let mut ids: Vec<&str> = PLAYBOOKS.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), PLAYBOOKS.len());
        for role in AgentRole::ALL {
            assert!(PLAYBOOKS.iter().any(|p| p.agents.contains(&role)), "{role}");
        }
    }
}
