use crate::a2a::{AgentCapabilities, AgentCard, AgentSkill};
use adk_core::Agent;
use adk_skill::SkillIndex;

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

/// Maps a [`SkillIndex`] to A2A agent-card `skills[]` entries.
///
/// Each indexed skill becomes one [`AgentSkill`]: the skill name maps to both
/// `id` and `name` (the stable, human-chosen identifier — the index's own
/// content-hash IDs change on every edit), the description maps to
/// `description`, and discovery tags map to `tags`. A skill version, when
/// present, is folded into `tags` as `version:{v}` because [`AgentSkill`] has
/// no version field — following the existing `sub_agent:{name}` prefixed-tag
/// convention. Agent Registry keyword/prefix search indexes these entries.
///
/// Wire via [`ServerBuilder::with_skill_index`](crate::ServerBuilder::with_skill_index)
/// or `A2aServerBuilder::skill_index` (behind the `a2a-v1` feature) to have the
/// served card include the entries.
///
/// # Example
///
/// ```
/// use adk_server::a2a::agent_skills_from_index;
/// use adk_skill::{SkillDocument, SkillIndex};
///
/// let doc = SkillDocument {
///     id: "search-expert-abc123def456".to_string(),
///     name: "search-expert".to_string(),
///     description: "Expert in semantic and keyword search.".to_string(),
///     version: Some("1.0.0".to_string()),
///     license: None,
///     compatibility: None,
///     tags: vec!["search".to_string()],
///     allowed_tools: vec![],
///     references: vec![],
///     trigger: false,
///     hint: None,
///     metadata: Default::default(),
///     body: String::new(),
///     path: "skills/search-expert.skill.md".into(),
///     hash: "abc123def456".to_string(),
///     last_modified: None,
///     triggers: vec![],
/// };
/// let index = SkillIndex::new(vec![doc]);
///
/// let skills = agent_skills_from_index(&index);
/// assert_eq!(skills[0].id, "search-expert");
/// assert_eq!(skills[0].tags, vec!["search".to_string(), "version:1.0.0".to_string()]);
/// ```
pub fn agent_skills_from_index(index: &SkillIndex) -> Vec<AgentSkill> {
    index
        .summaries()
        .into_iter()
        .map(|summary| {
            let mut tags = summary.tags;
            if let Some(version) = summary.version {
                tags.push(format!("version:{version}"));
            }
            AgentSkill::new(summary.name.clone(), summary.name, summary.description, tags)
        })
        .collect()
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
        .expect("build_agent_card: agent name, description, and url must be non-empty")
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

        async fn run(
            &self,
            _ctx: Arc<dyn adk_core::InvocationContext>,
        ) -> adk_core::Result<EventStream> {
            unimplemented!()
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }
    }

    #[test]
    fn test_build_agent_skills() {
        let agent =
            TestAgent { name: "test_agent".to_string(), description: "A test agent".to_string() };

        let skills = build_agent_skills(&agent);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].id, "test_agent");
        assert_eq!(skills[0].name, "test_agent");
    }

    #[test]
    fn test_build_agent_card() {
        let agent =
            TestAgent { name: "test_agent".to_string(), description: "A test agent".to_string() };

        let card = build_agent_card(&agent, "https://example.com");
        assert_eq!(card.name, "test_agent");
        assert_eq!(card.url, "https://example.com");
        assert!(card.capabilities.streaming);
    }

    fn skill_doc(
        name: &str,
        description: &str,
        version: Option<&str>,
        tags: &[&str],
    ) -> adk_skill::SkillDocument {
        adk_skill::SkillDocument {
            id: format!("{name}-0123456789ab"),
            name: name.to_string(),
            description: description.to_string(),
            version: version.map(str::to_string),
            license: None,
            compatibility: None,
            tags: tags.iter().map(|t| t.to_string()).collect(),
            allowed_tools: vec![],
            references: vec![],
            trigger: false,
            hint: None,
            metadata: Default::default(),
            body: String::new(),
            path: format!("skills/{name}.skill.md").into(),
            hash: "0123456789ab".to_string(),
            last_modified: None,
            triggers: vec![],
        }
    }

    #[test]
    fn test_agent_skills_from_index_two_skills_in_card_json() {
        let index = adk_skill::SkillIndex::new(vec![
            skill_doc(
                "search-expert",
                "Semantic and keyword search.",
                Some("1.0.0"),
                &["search", "retrieval"],
            ),
            skill_doc("code-reviewer", "Reviews Rust code for defects.", None, &["rust"]),
        ]);

        let card = AgentCard::builder()
            .name("test_agent".to_string())
            .description("A test agent".to_string())
            .url("https://example.com".to_string())
            .skills(agent_skills_from_index(&index))
            .build()
            .expect("card fields are non-empty");

        let expected = serde_json::json!({
            "name": "test_agent",
            "description": "A test agent",
            "url": "https://example.com",
            "version": "1.0.0",
            "protocolVersion": "0.3.0",
            "capabilities": {
                "streaming": false,
                "pushNotifications": false,
                "stateTransitionHistory": false,
            },
            "skills": [
                {
                    "id": "search-expert",
                    "name": "search-expert",
                    "description": "Semantic and keyword search.",
                    "tags": ["search", "retrieval", "version:1.0.0"],
                },
                {
                    "id": "code-reviewer",
                    "name": "code-reviewer",
                    "description": "Reviews Rust code for defects.",
                    "tags": ["rust"],
                },
            ],
        });

        assert_eq!(serde_json::to_value(&card).unwrap(), expected);
    }
}
