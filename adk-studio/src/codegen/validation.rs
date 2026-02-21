//! Workflow validation for code generation
//!
//! Validates project schemas before code generation to ensure:
//! - Connected graph structure
//! - Required fields are present
//! - Tool configurations are valid
//!
//! Requirements: 12.4, 12.5

use crate::schema::{AgentSchema, AgentType, END, ProjectSchema, START, ToolConfig};
use std::collections::{HashMap, HashSet};

/// Validation error with specific details
#[derive(Debug, Clone)]
pub struct ValidationError {
    /// Error code for categorization
    pub code: ValidationErrorCode,
    /// Human-readable error message
    pub message: String,
    /// Optional context (e.g., agent ID, field name)
    pub context: Option<String>,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(ctx) = &self.context {
            write!(f, "[{}] {}: {}", self.code, ctx, self.message)
        } else {
            write!(f, "[{}] {}", self.code, self.message)
        }
    }
}

/// Error codes for validation errors
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationErrorCode {
    /// No agents defined in the project
    NoAgents,
    /// No edges defined in the workflow
    NoEdges,
    /// Missing START edge
    MissingStartEdge,
    /// Missing END edge
    MissingEndEdge,
    /// Disconnected node (not reachable from START)
    DisconnectedNode,
    /// Missing required field
    MissingRequiredField,
    /// Invalid tool configuration
    InvalidToolConfig,
    /// Invalid route configuration
    InvalidRouteConfig,
    /// Circular dependency detected
    CircularDependency,
    /// Invalid sub-agent reference
    InvalidSubAgentRef,
}

impl std::fmt::Display for ValidationErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAgents => write!(f, "NO_AGENTS"),
            Self::NoEdges => write!(f, "NO_EDGES"),
            Self::MissingStartEdge => write!(f, "MISSING_START"),
            Self::MissingEndEdge => write!(f, "MISSING_END"),
            Self::DisconnectedNode => write!(f, "DISCONNECTED"),
            Self::MissingRequiredField => write!(f, "MISSING_FIELD"),
            Self::InvalidToolConfig => write!(f, "INVALID_TOOL"),
            Self::InvalidRouteConfig => write!(f, "INVALID_ROUTE"),
            Self::CircularDependency => write!(f, "CIRCULAR_DEP"),
            Self::InvalidSubAgentRef => write!(f, "INVALID_SUBAGENT"),
        }
    }
}

/// Result of workflow validation
#[derive(Debug)]
pub struct ValidationResult {
    /// List of validation errors
    pub errors: Vec<ValidationError>,
    /// List of validation warnings (non-blocking)
    pub warnings: Vec<ValidationError>,
}

impl ValidationResult {
    pub fn new() -> Self {
        Self { errors: Vec::new(), warnings: Vec::new() }
    }

    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }

    pub fn add_error(&mut self, code: ValidationErrorCode, message: impl Into<String>) {
        self.errors.push(ValidationError { code, message: message.into(), context: None });
    }

    pub fn add_error_with_context(
        &mut self,
        code: ValidationErrorCode,
        message: impl Into<String>,
        context: impl Into<String>,
    ) {
        self.errors.push(ValidationError {
            code,
            message: message.into(),
            context: Some(context.into()),
        });
    }

    pub fn add_warning(&mut self, code: ValidationErrorCode, message: impl Into<String>) {
        self.warnings.push(ValidationError { code, message: message.into(), context: None });
    }

    pub fn add_warning_with_context(
        &mut self,
        code: ValidationErrorCode,
        message: impl Into<String>,
        context: impl Into<String>,
    ) {
        self.warnings.push(ValidationError {
            code,
            message: message.into(),
            context: Some(context.into()),
        });
    }
}

impl Default for ValidationResult {
    fn default() -> Self {
        Self::new()
    }
}

/// Validate a project schema before code generation
///
/// Returns a ValidationResult containing any errors or warnings found.
/// Code generation should only proceed if `result.is_valid()` returns true.
pub fn validate_project(project: &ProjectSchema) -> ValidationResult {
    let mut result = ValidationResult::new();

    // Check for empty project
    validate_not_empty(project, &mut result);
    if !result.is_valid() {
        return result;
    }

    // Validate graph connectivity
    validate_graph_connectivity(project, &mut result);

    // Validate each agent
    for (agent_id, agent) in &project.agents {
        validate_agent(agent_id, agent, project, &mut result);
    }

    // Validate tool configurations
    validate_tool_configs(project, &mut result);

    result
}

/// Check that the project has at least one agent OR action node, and at least one edge
fn validate_not_empty(project: &ProjectSchema, result: &mut ValidationResult) {
    let has_agents = !project.agents.is_empty();
    let has_action_nodes = !project.action_nodes.is_empty();

    // Allow workflows with either agents OR action nodes
    if !has_agents && !has_action_nodes {
        result.add_error(
            ValidationErrorCode::NoAgents,
            "Project must have at least one agent or action node",
        );
    }

    if project.workflow.edges.is_empty() {
        result.add_error(ValidationErrorCode::NoEdges, "Workflow must have at least one edge");
    }
}

/// Validate that the graph is connected (all nodes reachable from START)
fn validate_graph_connectivity(project: &ProjectSchema, result: &mut ValidationResult) {
    // Build adjacency list
    let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut has_start_edge = false;
    let mut has_end_edge = false;

    for edge in &project.workflow.edges {
        if edge.from == START {
            has_start_edge = true;
        }
        if edge.to == END {
            has_end_edge = true;
        }
        adjacency.entry(edge.from.as_str()).or_default().push(edge.to.as_str());
    }

    // Check for START and END edges
    if !has_start_edge {
        result.add_error(
            ValidationErrorCode::MissingStartEdge,
            "Workflow must have an edge from START",
        );
    }

    if !has_end_edge {
        result.add_error(ValidationErrorCode::MissingEndEdge, "Workflow must have an edge to END");
    }

    // Find all top-level agents (not sub-agents)
    let all_sub_agents: HashSet<_> =
        project.agents.values().flat_map(|a| a.sub_agents.iter().map(|s| s.as_str())).collect();

    let top_level_agents: HashSet<_> =
        project.agents.keys().filter(|id| !all_sub_agents.contains(id.as_str())).collect();

    // BFS from START to find reachable nodes
    let mut reachable: HashSet<&str> = HashSet::new();
    let mut queue: Vec<&str> = vec![START];

    while let Some(node) = queue.pop() {
        if reachable.contains(node) {
            continue;
        }
        reachable.insert(node);

        if let Some(neighbors) = adjacency.get(node) {
            for neighbor in neighbors {
                if !reachable.contains(neighbor) {
                    queue.push(neighbor);
                }
            }
        }
    }

    // Check that all top-level agents are reachable
    // Note: Action nodes (like trigger) are also valid nodes in the graph
    for agent_id in &top_level_agents {
        // Skip if this agent is reachable through an action node
        // (e.g., START -> trigger -> agent)
        let reachable_through_action = project.action_nodes.keys().any(|action_id| {
            reachable.contains(action_id.as_str())
                && project.workflow.edges.iter().any(|e| e.from == *action_id && e.to == **agent_id)
        });

        if !reachable.contains(agent_id.as_str()) && !reachable_through_action {
            result.add_error_with_context(
                ValidationErrorCode::DisconnectedNode,
                "Agent is not reachable from START",
                agent_id.as_str(),
            );
        }
    }
}

/// Validate a single agent's configuration
fn validate_agent(
    agent_id: &str,
    agent: &AgentSchema,
    project: &ProjectSchema,
    result: &mut ValidationResult,
) {
    match agent.agent_type {
        AgentType::Llm => validate_llm_agent(agent_id, agent, result),
        AgentType::Router => validate_router_agent(agent_id, agent, project, result),
        AgentType::Sequential | AgentType::Loop | AgentType::Parallel => {
            validate_container_agent(agent_id, agent, project, result)
        }
        _ => {}
    }
}

/// Validate LLM agent configuration
fn validate_llm_agent(agent_id: &str, agent: &AgentSchema, result: &mut ValidationResult) {
    // LLM agents should have a model specified
    if agent.model.is_none() {
        result.add_warning_with_context(
            ValidationErrorCode::MissingRequiredField,
            "LLM agent has no model specified, will use default",
            agent_id,
        );
    }

    // Check for empty instruction (warning, not error)
    if agent.instruction.trim().is_empty() {
        result.add_warning_with_context(
            ValidationErrorCode::MissingRequiredField,
            "LLM agent has no instruction, behavior may be unpredictable",
            agent_id,
        );
    }
}

/// Validate router agent configuration
fn validate_router_agent(
    agent_id: &str,
    agent: &AgentSchema,
    project: &ProjectSchema,
    result: &mut ValidationResult,
) {
    // Router must have routes defined
    if agent.routes.is_empty() {
        result.add_error_with_context(
            ValidationErrorCode::InvalidRouteConfig,
            "Router agent must have at least one route defined",
            agent_id,
        );
        return;
    }

    // Validate each route target exists
    for route in &agent.routes {
        if route.target != END && !project.agents.contains_key(&route.target) {
            result.add_error_with_context(
                ValidationErrorCode::InvalidRouteConfig,
                format!("Route target '{}' does not exist", route.target),
                agent_id,
            );
        }

        // Check for empty condition
        if route.condition.trim().is_empty() {
            result.add_error_with_context(
                ValidationErrorCode::InvalidRouteConfig,
                "Route condition cannot be empty",
                agent_id,
            );
        }
    }
}

/// Validate container agent (Sequential, Loop, Parallel) configuration
fn validate_container_agent(
    agent_id: &str,
    agent: &AgentSchema,
    project: &ProjectSchema,
    result: &mut ValidationResult,
) {
    // Container must have sub-agents
    if agent.sub_agents.is_empty() {
        result.add_error_with_context(
            ValidationErrorCode::MissingRequiredField,
            format!("{:?} agent must have at least one sub-agent", agent.agent_type),
            agent_id,
        );
        return;
    }

    // Validate each sub-agent exists
    for sub_id in &agent.sub_agents {
        if !project.agents.contains_key(sub_id) {
            result.add_error_with_context(
                ValidationErrorCode::InvalidSubAgentRef,
                format!("Sub-agent '{}' does not exist", sub_id),
                agent_id,
            );
        }
    }

    // For loop agents, check max_iterations
    if agent.agent_type == AgentType::Loop {
        if let Some(max_iter) = agent.max_iterations {
            if max_iter == 0 {
                result.add_warning_with_context(
                    ValidationErrorCode::MissingRequiredField,
                    "Loop agent has max_iterations=0, will not execute",
                    agent_id,
                );
            }
        }
    }
}

/// Validate tool configurations
fn validate_tool_configs(project: &ProjectSchema, result: &mut ValidationResult) {
    for (tool_id, config) in &project.tool_configs {
        match config {
            ToolConfig::Mcp(mcp) => {
                if mcp.server_command.trim().is_empty() {
                    result.add_error_with_context(
                        ValidationErrorCode::InvalidToolConfig,
                        "MCP tool must have a server command",
                        tool_id,
                    );
                }
            }
            ToolConfig::Function(func) => {
                if func.name.trim().is_empty() {
                    result.add_error_with_context(
                        ValidationErrorCode::InvalidToolConfig,
                        "Function tool must have a name",
                        tool_id,
                    );
                }
                if func.description.trim().is_empty() {
                    result.add_warning_with_context(
                        ValidationErrorCode::InvalidToolConfig,
                        "Function tool has no description",
                        tool_id,
                    );
                }
            }
            ToolConfig::Browser(_) => {
                // Browser config has sensible defaults, no validation needed
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Edge, Position, Route};

    fn create_test_project() -> ProjectSchema {
        let mut project = ProjectSchema::new("test");
        project.agents.insert(
            "agent1".to_string(),
            AgentSchema {
                agent_type: AgentType::Llm,
                model: Some("gemini-2.5-flash".to_string()),
                instruction: "Test instruction".to_string(),
                tools: vec![],
                sub_agents: vec![],
                position: Position::default(),
                max_iterations: None,
                temperature: None,
                top_p: None,
                top_k: None,
                max_output_tokens: None,
                routes: vec![],
            },
        );
        project.workflow.edges = vec![Edge::new(START, "agent1"), Edge::new("agent1", END)];
        project
    }

    #[test]
    fn test_valid_project() {
        let project = create_test_project();
        let result = validate_project(&project);
        assert!(result.is_valid(), "Expected valid project, got errors: {:?}", result.errors);
    }

    #[test]
    fn test_empty_agents() {
        let mut project = create_test_project();
        project.agents.clear();
        let result = validate_project(&project);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.code == ValidationErrorCode::NoAgents));
    }

    #[test]
    fn test_empty_edges() {
        let mut project = create_test_project();
        project.workflow.edges.clear();
        let result = validate_project(&project);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.code == ValidationErrorCode::NoEdges));
    }

    #[test]
    fn test_missing_start_edge() {
        let mut project = create_test_project();
        project.workflow.edges = vec![Edge::new("agent1", END)];
        let result = validate_project(&project);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.code == ValidationErrorCode::MissingStartEdge));
    }

    #[test]
    fn test_missing_end_edge() {
        let mut project = create_test_project();
        project.workflow.edges = vec![Edge::new(START, "agent1")];
        let result = validate_project(&project);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.code == ValidationErrorCode::MissingEndEdge));
    }

    #[test]
    fn test_disconnected_node() {
        let mut project = create_test_project();
        project.agents.insert(
            "agent2".to_string(),
            AgentSchema {
                agent_type: AgentType::Llm,
                model: Some("gemini-2.5-flash".to_string()),
                instruction: "Disconnected".to_string(),
                tools: vec![],
                sub_agents: vec![],
                position: Position::default(),
                max_iterations: None,
                temperature: None,
                top_p: None,
                top_k: None,
                max_output_tokens: None,
                routes: vec![],
            },
        );
        // agent2 is not connected to the graph
        let result = validate_project(&project);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.code == ValidationErrorCode::DisconnectedNode));
    }

    #[test]
    fn test_router_without_routes() {
        let mut project = create_test_project();
        project.agents.insert(
            "router".to_string(),
            AgentSchema {
                agent_type: AgentType::Router,
                model: Some("gemini-2.5-flash".to_string()),
                instruction: "Route".to_string(),
                tools: vec![],
                sub_agents: vec![],
                position: Position::default(),
                max_iterations: None,
                temperature: None,
                top_p: None,
                top_k: None,
                max_output_tokens: None,
                routes: vec![], // No routes!
            },
        );
        project.workflow.edges = vec![Edge::new(START, "router"), Edge::new("router", END)];
        let result = validate_project(&project);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.code == ValidationErrorCode::InvalidRouteConfig));
    }

    #[test]
    fn test_router_with_invalid_target() {
        let mut project = create_test_project();
        project.agents.insert(
            "router".to_string(),
            AgentSchema {
                agent_type: AgentType::Router,
                model: Some("gemini-2.5-flash".to_string()),
                instruction: "Route".to_string(),
                tools: vec![],
                sub_agents: vec![],
                position: Position::default(),
                max_iterations: None,
                temperature: None,
                top_p: None,
                top_k: None,
                max_output_tokens: None,
                routes: vec![Route {
                    condition: "test".to_string(),
                    target: "nonexistent".to_string(), // Invalid target
                }],
            },
        );
        project.workflow.edges = vec![Edge::new(START, "router"), Edge::new("router", END)];
        let result = validate_project(&project);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.code == ValidationErrorCode::InvalidRouteConfig));
    }

    #[test]
    fn test_sequential_without_subagents() {
        let mut project = create_test_project();
        project.agents.insert(
            "seq".to_string(),
            AgentSchema {
                agent_type: AgentType::Sequential,
                model: None,
                instruction: String::new(),
                tools: vec![],
                sub_agents: vec![], // No sub-agents!
                position: Position::default(),
                max_iterations: None,
                temperature: None,
                top_p: None,
                top_k: None,
                max_output_tokens: None,
                routes: vec![],
            },
        );
        project.workflow.edges = vec![Edge::new(START, "seq"), Edge::new("seq", END)];
        let result = validate_project(&project);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.code == ValidationErrorCode::MissingRequiredField));
    }

    #[test]
    fn test_sequential_with_invalid_subagent() {
        let mut project = create_test_project();
        project.agents.insert(
            "seq".to_string(),
            AgentSchema {
                agent_type: AgentType::Sequential,
                model: None,
                instruction: String::new(),
                tools: vec![],
                sub_agents: vec!["nonexistent".to_string()], // Invalid sub-agent
                position: Position::default(),
                max_iterations: None,
                temperature: None,
                top_p: None,
                top_k: None,
                max_output_tokens: None,
                routes: vec![],
            },
        );
        project.workflow.edges = vec![Edge::new(START, "seq"), Edge::new("seq", END)];
        let result = validate_project(&project);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.code == ValidationErrorCode::InvalidSubAgentRef));
    }
}

/// Get a list of required environment variables for a project
///
/// This function analyzes the project schema and returns a list of
/// environment variables that must be set for the generated code to run.
///
/// Requirement: 12.10 - Warn when required env vars are missing
pub fn get_required_env_vars(project: &ProjectSchema) -> Vec<EnvVarRequirement> {
    let mut env_vars = Vec::new();

    // Detect which providers are used across all agents
    let providers = super::collect_providers(project);

    if providers.contains("gemini") {
        env_vars.push(EnvVarRequirement {
            name: "GOOGLE_API_KEY".to_string(),
            description: "Google AI API key for Gemini models".to_string(),
            alternatives: vec!["GEMINI_API_KEY".to_string()],
            required: true,
        });
    }

    if providers.contains("openai") {
        env_vars.push(EnvVarRequirement {
            name: "OPENAI_API_KEY".to_string(),
            description: "OpenAI API key for GPT models".to_string(),
            alternatives: vec![],
            required: true,
        });
    }

    if providers.contains("anthropic") {
        env_vars.push(EnvVarRequirement {
            name: "ANTHROPIC_API_KEY".to_string(),
            description: "Anthropic API key for Claude models".to_string(),
            alternatives: vec![],
            required: true,
        });
    }

    if providers.contains("deepseek") {
        env_vars.push(EnvVarRequirement {
            name: "DEEPSEEK_API_KEY".to_string(),
            description: "DeepSeek API key".to_string(),
            alternatives: vec![],
            required: true,
        });
    }

    if providers.contains("groq") {
        env_vars.push(EnvVarRequirement {
            name: "GROQ_API_KEY".to_string(),
            description: "Groq API key for fast inference".to_string(),
            alternatives: vec![],
            required: true,
        });
    }

    if providers.contains("ollama") {
        env_vars.push(EnvVarRequirement {
            name: "OLLAMA_HOST".to_string(),
            description: "Ollama server URL (defaults to http://localhost:11434)".to_string(),
            alternatives: vec![],
            required: false, // Ollama defaults to localhost
        });
    }

    if providers.contains("fireworks") {
        env_vars.push(EnvVarRequirement {
            name: "FIREWORKS_API_KEY".to_string(),
            description: "Fireworks AI API key".to_string(),
            alternatives: vec![],
            required: true,
        });
    }

    if providers.contains("together") {
        env_vars.push(EnvVarRequirement {
            name: "TOGETHER_API_KEY".to_string(),
            description: "Together AI API key".to_string(),
            alternatives: vec![],
            required: true,
        });
    }

    if providers.contains("mistral") {
        env_vars.push(EnvVarRequirement {
            name: "MISTRAL_API_KEY".to_string(),
            description: "Mistral AI API key".to_string(),
            alternatives: vec![],
            required: true,
        });
    }

    if providers.contains("perplexity") {
        env_vars.push(EnvVarRequirement {
            name: "PERPLEXITY_API_KEY".to_string(),
            description: "Perplexity API key for Sonar models".to_string(),
            alternatives: vec![],
            required: true,
        });
    }

    if providers.contains("cerebras") {
        env_vars.push(EnvVarRequirement {
            name: "CEREBRAS_API_KEY".to_string(),
            description: "Cerebras API key for ultra-fast inference".to_string(),
            alternatives: vec![],
            required: true,
        });
    }

    if providers.contains("sambanova") {
        env_vars.push(EnvVarRequirement {
            name: "SAMBANOVA_API_KEY".to_string(),
            description: "SambaNova API key".to_string(),
            alternatives: vec![],
            required: true,
        });
    }

    if providers.contains("bedrock") {
        env_vars.push(EnvVarRequirement {
            name: "AWS_ACCESS_KEY_ID".to_string(),
            description: "AWS credentials for Amazon Bedrock (or use IAM roles/SSO)".to_string(),
            alternatives: vec!["AWS_PROFILE".to_string()],
            required: true,
        });
        env_vars.push(EnvVarRequirement {
            name: "AWS_DEFAULT_REGION".to_string(),
            description: "AWS region for Bedrock (defaults to us-east-1)".to_string(),
            alternatives: vec!["AWS_REGION".to_string()],
            required: false,
        });
    }

    if providers.contains("azure-ai") {
        env_vars.push(EnvVarRequirement {
            name: "AZURE_AI_ENDPOINT".to_string(),
            description: "Azure AI Inference endpoint URL".to_string(),
            alternatives: vec![],
            required: true,
        });
        env_vars.push(EnvVarRequirement {
            name: "AZURE_AI_API_KEY".to_string(),
            description: "Azure AI API key".to_string(),
            alternatives: vec![],
            required: true,
        });
    }

    // Check for MCP tools that might need specific env vars
    for (tool_id, config) in &project.tool_configs {
        if let ToolConfig::Mcp(mcp) = config {
            // Common MCP servers that need env vars
            if mcp.server_command.contains("github")
                || mcp.server_args.iter().any(|a| a.contains("github"))
            {
                env_vars.push(EnvVarRequirement {
                    name: "GITHUB_TOKEN".to_string(),
                    description: format!("GitHub token for MCP server ({})", tool_id),
                    alternatives: vec!["GITHUB_PERSONAL_ACCESS_TOKEN".to_string()],
                    required: false, // May work without for public repos
                });
            }

            if mcp.server_command.contains("slack")
                || mcp.server_args.iter().any(|a| a.contains("slack"))
            {
                env_vars.push(EnvVarRequirement {
                    name: "SLACK_BOT_TOKEN".to_string(),
                    description: format!("Slack bot token for MCP server ({})", tool_id),
                    alternatives: vec![],
                    required: true,
                });
            }
        }
    }

    // Check for browser tool (may need headless browser setup)
    let uses_browser = project.agents.values().any(|a| a.tools.contains(&"browser".to_string()));
    if uses_browser {
        env_vars.push(EnvVarRequirement {
            name: "CHROME_PATH".to_string(),
            description: "Path to Chrome/Chromium executable (optional, auto-detected if not set)"
                .to_string(),
            alternatives: vec!["CHROMIUM_PATH".to_string()],
            required: false,
        });
    }

    env_vars
}

/// Environment variable requirement
#[derive(Debug, Clone)]
pub struct EnvVarRequirement {
    /// Primary environment variable name
    pub name: String,
    /// Description of what this variable is used for
    pub description: String,
    /// Alternative variable names that can be used
    pub alternatives: Vec<String>,
    /// Whether this variable is required (vs optional)
    pub required: bool,
}

impl EnvVarRequirement {
    /// Check if this environment variable is set
    pub fn is_set(&self) -> bool {
        if std::env::var(&self.name).is_ok() {
            return true;
        }
        self.alternatives.iter().any(|alt| std::env::var(alt).is_ok())
    }

    /// Get all possible variable names (primary + alternatives)
    pub fn all_names(&self) -> Vec<&str> {
        let mut names = vec![self.name.as_str()];
        names.extend(self.alternatives.iter().map(|s| s.as_str()));
        names
    }
}

/// Check for missing required environment variables
///
/// Returns a list of warnings for missing environment variables.
/// This is used to warn users before they try to run the generated code.
pub fn check_env_vars(project: &ProjectSchema) -> Vec<EnvVarWarning> {
    let requirements = get_required_env_vars(project);
    let mut warnings = Vec::new();

    for req in requirements {
        if !req.is_set() {
            warnings.push(EnvVarWarning {
                variable: req.name.clone(),
                description: req.description.clone(),
                alternatives: req.alternatives.clone(),
                required: req.required,
            });
        }
    }

    warnings
}

/// Warning about a missing environment variable
#[derive(Debug, Clone, serde::Serialize)]
pub struct EnvVarWarning {
    /// Primary environment variable name
    pub variable: String,
    /// Description of what this variable is used for
    pub description: String,
    /// Alternative variable names that can be used
    pub alternatives: Vec<String>,
    /// Whether this variable is required (vs optional)
    pub required: bool,
}

impl std::fmt::Display for EnvVarWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.required {
            write!(f, "Required: {} - {}", self.variable, self.description)?;
        } else {
            write!(f, "Optional: {} - {}", self.variable, self.description)?;
        }
        if !self.alternatives.is_empty() {
            write!(f, " (alternatives: {})", self.alternatives.join(", "))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod env_var_tests {
    use super::*;
    use crate::schema::{McpToolConfig, Position};

    #[test]
    fn test_gemini_requires_api_key() {
        let mut project = ProjectSchema::new("test");
        project.agents.insert(
            "agent".to_string(),
            AgentSchema {
                agent_type: AgentType::Llm,
                model: Some("gemini-2.5-flash".to_string()),
                instruction: "Test".to_string(),
                tools: vec![],
                sub_agents: vec![],
                position: Position::default(),
                max_iterations: None,
                temperature: None,
                top_p: None,
                top_k: None,
                max_output_tokens: None,
                routes: vec![],
            },
        );

        let env_vars = get_required_env_vars(&project);
        assert!(env_vars.iter().any(|v| v.name == "GOOGLE_API_KEY"));
    }

    #[test]
    fn test_browser_tool_env_var() {
        let mut project = ProjectSchema::new("test");
        project.agents.insert(
            "agent".to_string(),
            AgentSchema {
                agent_type: AgentType::Llm,
                model: Some("gemini-2.5-flash".to_string()),
                instruction: "Test".to_string(),
                tools: vec!["browser".to_string()],
                sub_agents: vec![],
                position: Position::default(),
                max_iterations: None,
                temperature: None,
                top_p: None,
                top_k: None,
                max_output_tokens: None,
                routes: vec![],
            },
        );

        let env_vars = get_required_env_vars(&project);
        assert!(env_vars.iter().any(|v| v.name == "CHROME_PATH"));
    }

    #[test]
    fn test_github_mcp_env_var() {
        let mut project = ProjectSchema::new("test");
        project.agents.insert(
            "agent".to_string(),
            AgentSchema {
                agent_type: AgentType::Llm,
                model: Some("gemini-2.5-flash".to_string()),
                instruction: "Test".to_string(),
                tools: vec!["mcp".to_string()],
                sub_agents: vec![],
                position: Position::default(),
                max_iterations: None,
                temperature: None,
                top_p: None,
                top_k: None,
                max_output_tokens: None,
                routes: vec![],
            },
        );
        project.tool_configs.insert(
            "agent_mcp".to_string(),
            ToolConfig::Mcp(McpToolConfig {
                server_command: "npx".to_string(),
                server_args: vec![
                    "-y".to_string(),
                    "@modelcontextprotocol/server-github".to_string(),
                ],
                tool_filter: vec![],
            }),
        );

        let env_vars = get_required_env_vars(&project);
        assert!(env_vars.iter().any(|v| v.name == "GITHUB_TOKEN"));
    }
}
