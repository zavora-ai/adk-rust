use crate::a2a::{AgentCapabilities, AgentCard, AgentSkill};
use adk_core::Agent;

pub fn build_agent_skills(agent: &dyn Agent) -> Vec<AgentSkill> {
    let mut skills = build_primary_skills(agent);
    skills.extend(build_sub_agent_skills(agent));
    skills
}

fn build_primary_skills(agent: &dyn Agent) -> Vec<AgentSkill> {
    vec![AgentSkill::new(
        agent.name().to_string(),
        agent.name().to_string(),
        agent.description().to_string(),
        vec!["agent".to_string()],
    )]
}

fn build_sub_agent_skills(agent: &dyn Agent) -> Vec<AgentSkill> {
    let sub_agents = agent.sub_agents();
    if sub_agents.is_empty() {
        return vec![];
    }

    let mut skills = vec![];

    // Add orchestration skill
    let descriptions: Vec<String> = sub_agents
        .iter()
        .map(|sub| {
            if sub.description().is_empty() {
                "No description".to_string()
            } else {
                sub.description().to_string()
            }
        })
        .collect();

    skills.push(AgentSkill::new(
        format!("{}-sub-agents", agent.name()),
        "sub-agents".to_string(),
        format!("Orchestrates: {}", descriptions.join("; ")),
        vec!["orchestration".to_string()],
    ));

    // Recursively add sub-agent skills
    for sub in sub_agents {
        let sub_skills = build_primary_skills(sub.as_ref());
        for skill in sub_skills {
            skills.push(AgentSkill::new(
                format!("{}_{}", sub.name(), skill.id),
                format!("{}: {}", sub.name(), skill.name),
                skill.description,
                {
                    let mut tags = vec![format!("sub_agent:{}", sub.name())];
                    tags.extend(skill.tags);
                    tags
                },
            ));
        }
    }

    skills
}

pub fn build_agent_card(agent: &dyn Agent, base_url: &str) -> AgentCard {
    AgentCard::builder()
        .name(agent.name().to_string())
        .description(agent.description().to_string())
        .url(base_url.to_string())
        .version("1.0.0".to_string())
        .capabilities(AgentCapabilities {
            streaming: true,
            push_notifications: false,
            state_transition_history: true,
            extensions: None,
        })
        .skills(build_agent_skills(agent))
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{Agent, EventStream};
    use async_trait::async_trait;
    use std::sync::Arc;

    struct TestAgent {
        name: String,
        description: String,
    }

    #[async_trait]
    impl Agent for TestAgent {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            &self.description
        }

        async fn run(&self, _ctx: Arc<dyn adk_core::InvocationContext>) -> adk_core::Result<EventStream> {
            unimplemented!()
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }
    }

    #[test]
    fn test_build_agent_skills() {
        let agent = TestAgent {
            name: "test_agent".to_string(),
            description: "A test agent".to_string(),
        };

        let skills = build_agent_skills(&agent);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].id, "test_agent");
        assert_eq!(skills[0].name, "test_agent");
    }

    #[test]
    fn test_build_agent_card() {
        let agent = TestAgent {
            name: "test_agent".to_string(),
            description: "A test agent".to_string(),
        };

        let card = build_agent_card(&agent, "https://example.com");
        assert_eq!(card.name, "test_agent");
        assert_eq!(card.url, "https://example.com");
        assert!(card.capabilities.streaming);
    }
}
