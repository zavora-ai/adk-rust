use crate::a2a::{AgentCapabilities, AgentCard, AgentSkill};
use adk_core::{Agent, AgentKind};
use std::sync::Arc;
use adk_skill::SkillIndex;

/// The skills an agent card lists for `agent`, as adk-go's `BuildAgentSkills` builds them:
/// the agent's own skills, then each sub-agent's, named after the sub-agent.
///
/// - An `LlmAgent` has a `model` skill whose description is its description, then its
///   fixed instruction and global instruction told in the first person ("you are" becomes
///   "I am"), tagged `llm`; and one skill per tool, `<agent>-<tool>`, tagged `llm` and `tools`.
/// - A workflow agent describes what it runs (`First, … Then, … Finally, …` for a sequence,
///   `… simultaneously.` in parallel, `… in a loop (max N iterations).`), tagged with its
///   kind (`sequential_workflow`, `parallel_workflow`, `loop_workflow`), and lists its
///   sub-agents in a `sub-agents` skill tagged `orchestration`. Any other agent is a
///   `custom_agent`.
/// - A sub-agent's skills are `<sub>_<id>`, named `<sub>: <name>`, tagged `sub_agent:<sub>`.
pub fn build_agent_skills(agent: &dyn Agent) -> Vec<AgentSkill> {
    let mut skills = build_primary_skills(agent);
    for sub in agent.sub_agents() {
        for skill in build_primary_skills(sub.as_ref()) {
            let mut tags = vec![format!("sub_agent:{}", sub.name())];
            tags.extend(skill.tags);
            skills.push(AgentSkill::new(
                format!("{}_{}", sub.name(), skill.id),
                format!("{}: {}", sub.name(), skill.name),
                skill.description,
                tags,
            ));
        }
    }
    skills
}

fn build_primary_skills(agent: &dyn Agent) -> Vec<AgentSkill> {
    let profile = agent.profile();
    if profile.kind == AgentKind::Llm {
        let mut parts: Vec<String> = Vec::new();
        if !agent.description().is_empty() {
            parts.push(agent.description().to_string());
        }
        parts.extend(profile.instruction.as_deref().map(first_person));
        parts.extend(profile.global_instruction.as_deref().map(first_person));
        let description =
            if parts.is_empty() { default_description(profile.kind) } else { parts.join(" ") };
        let mut skills = vec![AgentSkill::new(
            agent.name().to_string(),
            "model".to_string(),
            description,
            vec!["llm".to_string()],
        )];
        for tool in &profile.tools {
            let description = if tool.description.is_empty() {
                format!("Tool: {}", tool.name)
            } else {
                tool.description.clone()
            };
            skills.push(AgentSkill::new(
                format!("{}-{}", agent.name(), tool.name),
                tool.name.clone(),
                description,
                vec!["llm".to_string(), "tools".to_string()],
            ));
        }
        return skills;
    }

    let tag = kind_tag(profile.kind);
    let mut parts: Vec<String> = Vec::new();
    if !agent.description().is_empty() {
        parts.push(agent.description().to_string());
    }
    if !agent.sub_agents().is_empty()
        && let Some(flow) = workflow_description(agent, profile.kind)
    {
        parts.push(flow);
    }
    let description =
        if parts.is_empty() { default_description(profile.kind) } else { parts.join(" ") };
    let mut skills = vec![AgentSkill::new(
        agent.name().to_string(),
        skill_name(profile.kind).to_string(),
        description,
        vec![tag.to_string()],
    )];
    let sub_agents = agent.sub_agents();
    if !sub_agents.is_empty() {
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
            vec![tag.to_string(), "orchestration".to_string()],
        ));
    }
    skills
}

/// How a workflow agent runs its sub-agents, in words.
fn workflow_description(agent: &dyn Agent, kind: AgentKind) -> Option<String> {
    let subs = agent.sub_agents();
    let what = |sub: &Arc<dyn Agent>| {
        if sub.description().is_empty() {
            format!("execute the {} agent", sub.name())
        } else {
            sub.description().to_string()
        }
    };
    let last = subs.len() - 1;
    let listed = || {
        subs.iter()
            .enumerate()
            .map(|(i, sub)| match i {
                0 => format!("This agent will {}", what(sub)),
                i if i == last => format!("and {}", what(sub)),
                _ => format!(", {}", what(sub)),
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    match kind {
        AgentKind::Sequential => Some(
            subs.iter()
                .enumerate()
                .map(|(i, sub)| match i {
                    0 => format!("First, this agent will {}.", what(sub)),
                    i if i == last => format!("Finally, this agent will {}.", what(sub)),
                    _ => format!("Then, this agent will {}.", what(sub)),
                })
                .collect::<Vec<_>>()
                .join(" "),
        ),
        AgentKind::Parallel => Some(format!("{} simultaneously.", listed())),
        AgentKind::Loop { max_iterations } => {
            let max = max_iterations.map_or_else(|| "unlimited".to_string(), |max| max.to_string());
            Some(format!("{} in a loop (max {max} iterations).", listed()))
        }
        _ => None,
    }
}

fn default_description(kind: AgentKind) -> String {
    match kind {
        AgentKind::Loop { .. } => "A loop workflow agent",
        AgentKind::Sequential => "A sequential workflow agent",
        AgentKind::Parallel => "A parallel workflow agent",
        AgentKind::Llm => "An LLM-based agent",
        _ => "A custom agent",
    }
    .to_string()
}

fn kind_tag(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Loop { .. } => "loop_workflow",
        AgentKind::Sequential => "sequential_workflow",
        AgentKind::Parallel => "parallel_workflow",
        AgentKind::Llm => "llm_agent",
        _ => "custom_agent",
    }
}

fn skill_name(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Loop { .. } | AgentKind::Sequential | AgentKind::Parallel => "workflow",
        _ => "custom",
    }
}

/// An instruction told by the agent itself: "you are" becomes "I am", "your" becomes "my",
/// and so on, matching whole words in any case, as adk-go's `replacePronouns` does.
fn first_person(instruction: &str) -> String {
    const SUBSTITUTIONS: [(&str, &str); 7] = [
        ("you were", "I was"),
        ("you are", "I am"),
        ("you're", "I am"),
        ("you've", "I have"),
        ("yours", "mine"),
        ("your", "my"),
        ("you", "I"),
    ];
    let mut text = instruction.to_string();
    for (original, target) in SUBSTITUTIONS {
        text = replace_word(&text, original, target);
    }
    text
}

/// `text` with each whole-word, case-insensitive match of `word` replaced by `with`.
fn replace_word(text: &str, word: &str, with: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(found) = lower[i..].find(word) {
        let start = i + found;
        let end = start + word.len();
        let before = text[..start].chars().next_back().is_none_or(|c| !is_word(c));
        let after = text[end..].chars().next().is_none_or(|c| !is_word(c));
        if before && after {
            out.push_str(&text[i..start]);
            out.push_str(with);
        } else {
            out.push_str(&text[i..end]);
        }
        i = end;
    }
    out.push_str(&text[i..]);
    out
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
        assert_eq!(skills[0].name, "custom", "an agent of its own kind is a custom agent");
        assert_eq!(skills[0].tags, ["custom_agent"]);
    }

    #[test]
    fn an_llm_agent_lists_its_model_and_each_tool_as_adk_go_does() {
        struct AddTool;

        #[async_trait]
        impl adk_core::Tool for AddTool {
            fn name(&self) -> &str {
                "add"
            }

            fn description(&self) -> &str {
                "Adds two integers."
            }

            async fn execute(
                &self,
                _ctx: Arc<dyn adk_core::ToolContext>,
                _args: serde_json::Value,
            ) -> adk_core::Result<serde_json::Value> {
                Ok(serde_json::json!({}))
            }
        }
        let tool = Arc::new(AddTool);
        let agent = adk_agent::LlmAgentBuilder::new("tools_agent")
            .description("Agent with the shared tool library.")
            .instruction("You are precise. Use your tools.")
            .model(Arc::new(adk_model::MockLlm::new("m")))
            .tool(tool)
            .build()
            .unwrap();

        let skills = build_agent_skills(&agent);
        assert_eq!(skills.len(), 2);
        assert_eq!((skills[0].id.as_str(), skills[0].name.as_str()), ("tools_agent", "model"));
        assert_eq!(
            skills[0].description,
            "Agent with the shared tool library. I am precise. Use my tools."
        );
        assert_eq!(skills[0].tags, ["llm"]);
        assert_eq!((skills[1].id.as_str(), skills[1].name.as_str()), ("tools_agent-add", "add"));
        assert_eq!(skills[1].tags, ["llm", "tools"]);
    }

    #[test]
    fn a_workflow_agent_describes_what_it_runs() {
        let step = |name: &str, description: &str| -> Arc<dyn Agent> {
            Arc::new(TestAgent { name: name.to_string(), description: description.to_string() })
        };
        let sequence = adk_agent::SequentialAgent::new(
            "pipeline",
            vec![step("draft", "write a draft"), step("check", ""), step("ship", "publish it")],
        );
        let skills = build_agent_skills(&sequence);
        assert_eq!(skills[0].name, "workflow");
        assert_eq!(
            skills[0].description,
            "First, this agent will write a draft. Then, this agent will execute the check agent. \
             Finally, this agent will publish it."
        );
        assert_eq!(skills[0].tags, ["sequential_workflow"]);
        assert_eq!(skills[1].id, "pipeline-sub-agents");
        assert_eq!(skills[1].tags, ["sequential_workflow", "orchestration"]);
        assert_eq!(skills[2].id, "draft_draft");
        assert_eq!(skills[2].name, "draft: custom");
        assert_eq!(skills[2].tags, ["sub_agent:draft", "custom_agent"]);
    }

    #[test]
    fn instructions_are_told_in_the_first_person() {
        assert_eq!(
            first_person("You are helpful. Your answers are yours; you've seen youth."),
            "I am helpful. my answers are mine; I have seen youth."
        );
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
