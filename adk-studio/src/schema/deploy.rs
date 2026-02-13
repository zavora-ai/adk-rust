use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{AgentType, ProjectSchema, START, ToolConfig, ToolType, WorkflowType};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployManifest {
    pub schema_version: String,
    pub generated_at: DateTime<Utc>,
    pub source: DeploySource,
    pub app: SpatialAppManifest,
    pub runtime: DeployRuntime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploySource {
    pub kind: String,
    pub project_id: String,
    pub project_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpatialAppManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handoff_allowlist: Vec<String>,
    pub default_risk: DeployRiskTier,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub starter_prompts: Vec<String>,
    #[serde(default)]
    pub runtime: SpatialAppRuntime,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SpatialAppRuntime {
    #[serde(default = "default_runtime_mode")]
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
    pub supports_sub_agents: bool,
    pub supports_a2a: bool,
    #[serde(default)]
    pub callback_mode: DeployCallbackMode,
    #[serde(default)]
    pub tool_confirmation_policy: DeployToolConfirmationPolicy,
    #[serde(default)]
    pub guardrails: DeployGuardrailPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeployCallbackMode {
    #[default]
    Off,
    Observe,
    Enforce,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeployToolConfirmationPolicy {
    #[default]
    Never,
    Always,
    PerTool(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeployGuardrailProfile {
    HarmfulContent,
    PiiRedaction,
    #[serde(rename = "output_max_length_2000")]
    OutputMaxLength2000,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DeployGuardrailPolicy {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_profiles: Vec<DeployGuardrailProfile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_profiles: Vec<DeployGuardrailProfile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeployRiskTier {
    Safe,
    Controlled,
    Dangerous,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployRuntime {
    pub mode: String,
    pub workflow_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    pub supports_sub_agents: bool,
    pub supports_a2a: bool,
}

impl DeployManifest {
    pub fn from_project(project: &ProjectSchema) -> Self {
        let capabilities = infer_capabilities(project);
        let permissions = infer_permissions(project);
        let risk = infer_risk_tier(project, &permissions);
        let root_agent_id = infer_root_agent_id(project);
        let default_model = infer_default_model(project);
        let default_provider = infer_default_provider(project);
        let app_id = build_app_id(project);
        let starter_prompts = infer_starter_prompts(project, root_agent_id.as_deref());
        let root_instruction = root_agent_id
            .as_ref()
            .and_then(|agent_id| project.agents.get(agent_id))
            .map(|agent| agent.instruction.trim().to_string())
            .filter(|instruction| !instruction.is_empty());

        Self {
            schema_version: "adk-spatial-os-deploy/v1".to_string(),
            generated_at: Utc::now(),
            source: DeploySource {
                kind: "adk_studio".to_string(),
                project_id: project.id.to_string(),
                project_name: project.name.clone(),
            },
            app: SpatialAppManifest {
                id: app_id,
                name: project.name.clone(),
                version: normalize_version(&project.version),
                description: normalize_description(project),
                capabilities,
                permissions,
                handoff_allowlist: vec![],
                default_risk: risk,
                starter_prompts,
                runtime: SpatialAppRuntime {
                    mode: "adk_runner".to_string(),
                    workflow_type: Some(
                        workflow_type_label(&project.workflow.workflow_type).to_string(),
                    ),
                    root_agent_id: root_agent_id.clone(),
                    default_model: default_model.clone(),
                    provider: default_provider.clone(),
                    instruction: root_instruction,
                    supports_sub_agents: project
                        .agents
                        .values()
                        .any(|agent| !agent.sub_agents.is_empty()),
                    supports_a2a: false,
                    callback_mode: DeployCallbackMode::Observe,
                    tool_confirmation_policy: DeployToolConfirmationPolicy::Never,
                    guardrails: DeployGuardrailPolicy {
                        input_profiles: vec![
                            DeployGuardrailProfile::HarmfulContent,
                            DeployGuardrailProfile::PiiRedaction,
                        ],
                        output_profiles: vec![
                            DeployGuardrailProfile::PiiRedaction,
                            DeployGuardrailProfile::OutputMaxLength2000,
                        ],
                    },
                },
            },
            runtime: DeployRuntime {
                mode: "adk_runner".to_string(),
                workflow_type: workflow_type_label(&project.workflow.workflow_type).to_string(),
                root_agent_id,
                default_model,
                supports_sub_agents: project
                    .agents
                    .values()
                    .any(|agent| !agent.sub_agents.is_empty()),
                supports_a2a: false,
            },
        }
    }
}

fn build_app_id(project: &ProjectSchema) -> String {
    let slug = project
        .name
        .to_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let prefix = if slug.is_empty() { "studio-app".to_string() } else { slug };
    let short_id = project.id.to_string().chars().take(8).collect::<String>();
    format!("{prefix}-{short_id}")
}

fn normalize_version(version: &str) -> String {
    if version.trim().is_empty() { "1.0.0".to_string() } else { version.to_string() }
}

fn normalize_description(project: &ProjectSchema) -> String {
    if project.description.trim().is_empty() {
        format!("Deployed from ADK Studio project {}", project.name)
    } else {
        project.description.clone()
    }
}

fn default_runtime_mode() -> String {
    "adk_runner".to_string()
}

fn workflow_type_label(workflow_type: &WorkflowType) -> &'static str {
    match workflow_type {
        WorkflowType::Single => "single",
        WorkflowType::Sequential => "sequential",
        WorkflowType::Parallel => "parallel",
        WorkflowType::Graph => "graph",
    }
}

fn agent_type_label(agent_type: &AgentType) -> &'static str {
    match agent_type {
        AgentType::Llm => "llm",
        AgentType::Tool => "tool",
        AgentType::Sequential => "sequential",
        AgentType::Parallel => "parallel",
        AgentType::Loop => "loop",
        AgentType::Router => "router",
        AgentType::Graph => "graph",
        AgentType::Custom => "custom",
    }
}

fn infer_root_agent_id(project: &ProjectSchema) -> Option<String> {
    project
        .workflow
        .edges
        .iter()
        .find(|edge| edge.from == START)
        .map(|edge| edge.to.clone())
        .or_else(|| project.agents.keys().next().cloned())
}

fn infer_default_model(project: &ProjectSchema) -> Option<String> {
    project
        .agents
        .values()
        .find_map(|agent| agent.model.clone())
        .or_else(|| Some(project.settings.default_model.clone()))
}

fn infer_default_provider(project: &ProjectSchema) -> Option<String> {
    project
        .settings
        .default_provider
        .as_ref()
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty())
        .map(|provider| {
            if provider.contains("vertex") {
                "vertex_adc".to_string()
            } else {
                "ai_studio".to_string()
            }
        })
}

fn infer_capabilities(project: &ProjectSchema) -> Vec<String> {
    let mut capabilities = BTreeSet::new();
    capabilities
        .insert(format!("workflow:{}", workflow_type_label(&project.workflow.workflow_type)));

    for (agent_id, agent) in &project.agents {
        capabilities.insert(format!("agent:{}", agent_type_label(&agent.agent_type)));
        capabilities.insert(format!("agent-id:{agent_id}"));
        for tool in &agent.tools {
            capabilities.insert(format!("tool:{tool}"));
        }
    }

    for tool_name in project.tools.keys() {
        capabilities.insert(format!("tool:{tool_name}"));
    }
    for tool_schema in project.tools.values() {
        let kind = match tool_schema.tool_type {
            ToolType::Builtin => "builtin",
            ToolType::Mcp => "mcp",
            ToolType::Custom => "custom",
        };
        capabilities.insert(format!("tool-kind:{kind}"));
    }
    for action in project.action_nodes.values() {
        capabilities.insert(format!("action:{}", action.node_type()));
    }

    capabilities.into_iter().collect()
}

fn infer_permissions(project: &ProjectSchema) -> Vec<String> {
    let mut permissions = BTreeSet::new();
    permissions.insert("runner:invoke".to_string());

    for agent in project.agents.values() {
        for tool in &agent.tools {
            apply_tool_permission(tool, &mut permissions);
        }
    }

    for tool in project.tools.keys() {
        apply_tool_permission(tool, &mut permissions);
    }

    for config in project.tool_configs.values() {
        match config {
            ToolConfig::Mcp(_) => {
                permissions.insert("tool:mcp:invoke".to_string());
            }
            ToolConfig::Function(_) => {
                permissions.insert("tool:function:invoke".to_string());
            }
            ToolConfig::Browser(_) => {
                permissions.insert("net:web:read".to_string());
            }
        }
    }

    for action in project.action_nodes.values() {
        match action.node_type() {
            "http" => {
                permissions.insert("net:http:request".to_string());
            }
            "database" => {
                permissions.insert("db:access".to_string());
            }
            "email" => {
                permissions.insert("comm:email:send".to_string());
            }
            "notification" => {
                permissions.insert("comm:notify:send".to_string());
            }
            "file" => {
                permissions.insert("fs:read_write".to_string());
            }
            "code" => {
                permissions.insert("code:execute".to_string());
            }
            _ => {}
        }
    }

    permissions.into_iter().collect()
}

fn apply_tool_permission(tool: &str, permissions: &mut BTreeSet<String>) {
    match tool {
        "google_search" => {
            permissions.insert("net:search:read".to_string());
        }
        "browser" | "web_browse" => {
            permissions.insert("net:web:read".to_string());
        }
        "code_exec" => {
            permissions.insert("code:execute".to_string());
        }
        "file_read" | "file_write" => {
            permissions.insert("fs:read_write".to_string());
        }
        _ => {}
    }
}

fn infer_risk_tier(project: &ProjectSchema, permissions: &[String]) -> DeployRiskTier {
    let has_high_risk_action = project
        .action_nodes
        .values()
        .any(|action| matches!(action.node_type(), "code" | "database" | "file"));
    if has_high_risk_action || permissions.iter().any(|perm| perm == "tool:mcp:invoke") {
        return DeployRiskTier::Dangerous;
    }

    let has_controlled_actions = project.action_nodes.values().any(|action| {
        matches!(action.node_type(), "http" | "email" | "notification" | "set" | "transform")
    });
    if has_controlled_actions || permissions.len() > 1 {
        DeployRiskTier::Controlled
    } else {
        DeployRiskTier::Safe
    }
}

fn infer_starter_prompts(project: &ProjectSchema, root_agent_id: Option<&str>) -> Vec<String> {
    let mut prompts = Vec::new();
    prompts.push(format!("Run {} and summarize the highest-priority outcomes.", project.name));

    if let Some(root_id) = root_agent_id {
        if let Some(root_agent) = project.agents.get(root_id) {
            let instruction = root_agent.instruction.trim();
            if !instruction.is_empty() {
                let first_sentence =
                    instruction.split_terminator('.').next().unwrap_or(instruction).trim();
                if !first_sentence.is_empty() {
                    prompts.push(format!("Follow this objective: {first_sentence}."));
                }
            }
            if !root_agent.tools.is_empty() {
                let tool_preview = root_agent.tools.iter().take(3).cloned().collect::<Vec<_>>();
                prompts.push(format!(
                    "Use {} and return a concise action plan.",
                    tool_preview.join(", ")
                ));
            }
        }
    }

    if prompts.len() < 3 {
        prompts.push(format!("Provide a safe, step-by-step execution plan for {}.", project.name));
    }

    prompts.into_iter().take(3).collect()
}

#[cfg(test)]
mod tests {
    use super::{DeployManifest, DeployRiskTier};
    use crate::schema::{AgentSchema, ProjectSchema, START, WorkflowType};

    #[test]
    fn deploy_manifest_infers_root_agent_and_capabilities() {
        let mut project = ProjectSchema::new("Support Ops");
        project.workflow.workflow_type = WorkflowType::Sequential;
        project.agents.insert(
            "router".to_string(),
            AgentSchema::llm("gemini-2.5-flash")
                .with_instruction("route support tickets")
                .with_tools(vec!["google_search".to_string()]),
        );
        project.workflow.edges.push(crate::schema::Edge::new(START, "router"));

        let manifest = DeployManifest::from_project(&project);
        assert!(manifest.app.id.starts_with("support-ops-"));
        assert_eq!(manifest.runtime.root_agent_id.as_deref(), Some("router"));
        assert!(manifest.app.capabilities.iter().any(|cap| cap == "workflow:sequential"));
        assert_eq!(manifest.app.default_risk, DeployRiskTier::Controlled);
    }

    #[test]
    fn deploy_manifest_marks_dangerous_for_high_risk_action_nodes() {
        let payload = r#"{
          "type": "code",
          "id": "run_js",
          "name": "Run JS",
          "description": "exec code",
          "language": "javascript",
          "script": "return {}",
          "errorHandling": {"mode":"stop"},
          "tracing": {"enabled": false, "logLevel":"error"},
          "callbacks": {},
          "execution": {"timeout": 30000},
          "mapping": {"outputKey":"result"}
        }"#;
        let code_node: crate::codegen::action_node_types::ActionNodeConfig =
            serde_json::from_str(payload).expect("valid code action node");

        let mut project = ProjectSchema::new("Risky");
        project.action_nodes.insert("run_js".to_string(), code_node);

        let manifest = DeployManifest::from_project(&project);
        assert_eq!(manifest.app.default_risk, DeployRiskTier::Dangerous);
    }
}
