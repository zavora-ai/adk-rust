//! Rust code generation from project schemas - Always uses adk-graph
//!
//! This module provides code generation for ADK Studio projects, converting
//! visual workflow definitions into compilable Rust code using adk-graph.
//!
//! ## Features
//!
//! - Workflow validation before code generation (Requirements 12.4, 12.5)
//! - Explanatory comments in generated code (Requirement 12.2)
//! - Environment variable warnings (Requirement 12.10)
//! - Support for all agent types: LLM, Sequential, Loop, Parallel, Router
//! - Action node code generation (Requirements 13.1, 13.2, 13.3)

pub mod action_node_codegen;
pub mod action_node_types;
mod validation;

// Backward-compatible re-export so `crate::codegen::action_nodes::*` still works
pub use action_node_codegen as action_nodes;

pub use validation::{
    EnvVarRequirement, EnvVarWarning, ValidationError, ValidationErrorCode, ValidationResult,
    check_env_vars, get_required_env_vars, validate_project,
};

use crate::schema::{AgentSchema, AgentType, ProjectSchema, ToolConfig};
use anyhow::{Result, bail};

/// Generate a Rust project from a project schema
///
/// This function validates the project before generating code. If validation
/// fails, it returns an error with details about what needs to be fixed.
///
/// # Arguments
///
/// * `project` - The project schema to generate code from
///
/// # Returns
///
/// * `Ok(GeneratedProject)` - The generated project files
/// * `Err` - If validation fails or code generation encounters an error
///
/// # Requirements
///
/// - 12.1: Generate valid, compilable ADK-Rust code
/// - 12.4: Validate workflow before code generation
/// - 12.5: Display specific error messages if validation fails
pub fn generate_rust_project(project: &ProjectSchema) -> Result<GeneratedProject> {
    // Validate the project before generating code (Requirements 12.4, 12.5)
    let validation = validate_project(project);
    if !validation.is_valid() {
        let error_messages: Vec<String> = validation.errors.iter().map(|e| e.to_string()).collect();
        bail!("Workflow validation failed:\n{}", error_messages.join("\n"));
    }

    let files = vec![
        GeneratedFile { path: "src/main.rs".to_string(), content: generate_main_rs(project) },
        GeneratedFile { path: "Cargo.toml".to_string(), content: generate_cargo_toml(project) },
    ];

    Ok(GeneratedProject { files })
}

/// Generate a Rust project with validation result
///
/// This function returns both the generated project and the validation result,
/// allowing callers to access warnings even when generation succeeds.
pub fn generate_rust_project_with_validation(
    project: &ProjectSchema,
) -> Result<(GeneratedProject, ValidationResult, Vec<EnvVarWarning>)> {
    let validation = validate_project(project);
    if !validation.is_valid() {
        let error_messages: Vec<String> = validation.errors.iter().map(|e| e.to_string()).collect();
        bail!("Workflow validation failed:\n{}", error_messages.join("\n"));
    }

    // Check for missing environment variables (Requirement 12.10)
    let env_warnings = check_env_vars(project);

    let files = vec![
        GeneratedFile { path: "src/main.rs".to_string(), content: generate_main_rs(project) },
        GeneratedFile { path: "Cargo.toml".to_string(), content: generate_cargo_toml(project) },
    ];

    Ok((GeneratedProject { files }, validation, env_warnings))
}

/// Generate a header comment explaining the workflow structure
///
/// This creates a comprehensive comment block at the top of the generated code
/// that explains:
/// - Project name and description
/// - Workflow type and structure
/// - Agent roles and their connections
/// - Tools used by each agent
///
/// Requirement: 12.2 - Include comments explaining the workflow structure
fn generate_workflow_header_comment(project: &ProjectSchema) -> String {
    let mut comment = String::new();

    // Project header
    comment.push_str("//! ");
    comment.push_str(&project.name);
    comment.push_str("\n//!\n");

    if !project.description.is_empty() {
        comment.push_str("//! ");
        comment.push_str(&project.description);
        comment.push_str("\n//!\n");
    }

    // Workflow structure overview
    comment.push_str("//! ## Workflow Structure\n//!\n");

    // Determine workflow type
    let workflow_type = determine_workflow_type(project);
    comment.push_str("//! **Type:** ");
    comment.push_str(&workflow_type);
    comment.push_str("\n//!\n");

    // Agent summary
    comment.push_str("//! ## Agents\n//!\n");

    // Find top-level agents (not sub-agents)
    let all_sub_agents: std::collections::HashSet<_> =
        project.agents.values().flat_map(|a| a.sub_agents.iter().cloned()).collect();

    for (agent_id, agent) in &project.agents {
        let is_sub_agent = all_sub_agents.contains(agent_id);
        let prefix = if is_sub_agent { "  - " } else { "- " };

        comment.push_str("//! ");
        comment.push_str(prefix);
        comment.push_str("**");
        comment.push_str(agent_id);
        comment.push_str("** (");
        comment.push_str(&format!("{:?}", agent.agent_type).to_lowercase());
        comment.push_str(")");

        // Add brief instruction summary if available
        if !agent.instruction.is_empty() {
            let brief = truncate_instruction(&agent.instruction, 60);
            comment.push_str(": ");
            comment.push_str(&brief);
        }
        comment.push_str("\n");

        // List tools if any
        if !agent.tools.is_empty() {
            comment.push_str("//!   Tools: ");
            comment.push_str(&agent.tools.join(", "));
            comment.push_str("\n");
        }

        // List sub-agents if any
        if !agent.sub_agents.is_empty() {
            comment.push_str("//!   Sub-agents: ");
            comment.push_str(&agent.sub_agents.join(" → "));
            comment.push_str("\n");
        }
    }

    // Execution flow
    comment.push_str("//!\n//! ## Execution Flow\n//!\n");
    comment.push_str("//! ```text\n");
    comment.push_str(&generate_flow_diagram(project));
    comment.push_str("//! ```\n//!\n");

    // Generated timestamp
    comment.push_str("//! Generated by ADK Studio v2.0\n//!\n");

    // Environment variables section (Requirement 12.10)
    let env_vars = get_required_env_vars(project);
    if !env_vars.is_empty() {
        comment.push_str("//! ## Required Environment Variables\n//!\n");
        for env_var in &env_vars {
            if env_var.required {
                comment.push_str("//! - **");
            } else {
                comment.push_str("//! - ");
            }
            comment.push_str(&env_var.name);
            if env_var.required {
                comment.push_str("** (required)");
            } else {
                comment.push_str(" (optional)");
            }
            comment.push_str(": ");
            comment.push_str(&env_var.description);
            if !env_var.alternatives.is_empty() {
                comment.push_str(" [alt: ");
                comment.push_str(&env_var.alternatives.join(", "));
                comment.push_str("]");
            }
            comment.push_str("\n");
        }
        comment.push_str("//!\n");
    }

    comment
}

/// Determine the workflow type based on agents and edges
fn determine_workflow_type(project: &ProjectSchema) -> String {
    let has_router = project.agents.values().any(|a| a.agent_type == AgentType::Router);
    let has_loop = project.agents.values().any(|a| a.agent_type == AgentType::Loop);
    let has_parallel = project.agents.values().any(|a| a.agent_type == AgentType::Parallel);
    let has_sequential = project.agents.values().any(|a| a.agent_type == AgentType::Sequential);

    // Find top-level agents
    let all_sub_agents: std::collections::HashSet<_> =
        project.agents.values().flat_map(|a| a.sub_agents.iter().cloned()).collect();
    let top_level_count = project.agents.keys().filter(|id| !all_sub_agents.contains(*id)).count();

    if has_router {
        "Router-based workflow with conditional branching".to_string()
    } else if has_loop {
        "Iterative loop workflow with refinement".to_string()
    } else if has_parallel {
        "Parallel execution workflow".to_string()
    } else if has_sequential || top_level_count > 1 {
        "Sequential pipeline workflow".to_string()
    } else {
        "Single agent workflow".to_string()
    }
}

/// Truncate instruction to a brief summary
fn truncate_instruction(instruction: &str, max_len: usize) -> String {
    let clean = instruction.replace('\n', " ").trim().to_string();
    if clean.len() <= max_len {
        clean
    } else {
        format!("{}...", &clean[..max_len.saturating_sub(3)])
    }
}

/// Strip {{var}} template variables from instruction text
///
/// This is needed because the agent's template system looks at session state,
/// but Set nodes update graph state. By stripping the template variables from
/// the instruction and injecting them via the input_mapper instead, we ensure
/// the variables are properly resolved from graph state.
fn strip_template_variables(instruction: &str) -> String {
    let mut result = instruction.to_string();
    // Find and remove all {{var}} patterns
    while let Some(start) = result.find("{{") {
        if let Some(end) = result[start..].find("}}") {
            let end_pos = start + end + 2;
            result = format!("{}{}", &result[..start], &result[end_pos..]);
        } else {
            break;
        }
    }
    // Clean up any double spaces left behind
    while result.contains("  ") {
        result = result.replace("  ", " ");
    }
    result.trim().to_string()
}

/// Generate a simple ASCII flow diagram
fn generate_flow_diagram(project: &ProjectSchema) -> String {
    let mut diagram = String::new();

    // Build a simple linear representation of the flow
    let mut current = "START";
    let mut visited = std::collections::HashSet::new();

    diagram.push_str("//! START");

    while current != "END" && !visited.contains(current) {
        visited.insert(current);

        // Find next node(s)
        let next_edges: Vec<_> =
            project.workflow.edges.iter().filter(|e| e.from == current).collect();

        if next_edges.is_empty() {
            break;
        }

        if next_edges.len() == 1 {
            let next = &next_edges[0].to;
            // Skip action nodes in the diagram (they're entry points, not execution nodes)
            if project.action_nodes.contains_key(next) {
                current = next;
                continue;
            }
            diagram.push_str(" → ");
            diagram.push_str(next);
            current = next;
        } else {
            // Multiple branches (router)
            diagram.push_str(" → [");
            let targets: Vec<_> = next_edges
                .iter()
                .map(|e| e.to.as_str())
                .filter(|t| !project.action_nodes.contains_key(*t))
                .collect();
            diagram.push_str(&targets.join(" | "));
            diagram.push_str("]");
            break; // Stop at branching point
        }
    }

    diagram.push_str("\n");
    diagram
}

#[derive(Debug, serde::Serialize)]
pub struct GeneratedProject {
    pub files: Vec<GeneratedFile>,
}

#[derive(Debug, serde::Serialize)]
pub struct GeneratedFile {
    pub path: String,
    pub content: String,
}

fn generate_main_rs(project: &ProjectSchema) -> String {
    let mut code = String::new();

    // Generate file header with workflow documentation (Requirement 12.2)
    code.push_str(&generate_workflow_header_comment(project));

    code.push_str("#![allow(unused_imports, unused_variables)]\n\n");

    // Check if any agent uses MCP (handles mcp, mcp_1, mcp_2, etc.)
    let uses_mcp = project
        .agents
        .values()
        .any(|a| a.tools.iter().any(|t| t == "mcp" || t.starts_with("mcp_")));
    let uses_browser = project.agents.values().any(|a| a.tools.contains(&"browser".to_string()));

    // Graph imports
    code.push_str("use adk_agent::LlmAgentBuilder;\n");
    code.push_str("use adk_core::ToolContext;\n");
    code.push_str("use adk_graph::{\n");
    code.push_str("    edge::{Router, END, START},\n");
    code.push_str("    graph::StateGraph,\n");
    code.push_str("    node::{AgentNode, ExecutionConfig, NodeOutput},\n");
    code.push_str("    state::State,\n");
    code.push_str("    StreamEvent,\n");
    code.push_str("};\n");
    code.push_str("use adk_model::gemini::GeminiModel;\n");
    code.push_str(
        "use adk_tool::{FunctionTool, GoogleSearchTool, ExitLoopTool, LoadArtifactsTool};\n",
    );
    if uses_mcp || uses_browser {
        code.push_str("use adk_core::{ReadonlyContext, Toolset, Content};\n");
    }
    if uses_mcp {
        code.push_str("use adk_tool::McpToolset;\n");
        code.push_str("use rmcp::{ServiceExt, transport::TokioChildProcess};\n");
        code.push_str("use tokio::process::Command;\n");
    }
    if uses_mcp || uses_browser {
        code.push_str("use async_trait::async_trait;\n");
    }
    if uses_browser {
        code.push_str("use adk_browser::{BrowserSession, BrowserConfig, BrowserToolset};\n");
    }
    code.push_str("use anyhow::Result;\n");
    code.push_str("use serde_json::{json, Value};\n");
    code.push_str("use std::sync::Arc;\n");
    code.push_str("use tracing_subscriber::{fmt, EnvFilter};\n\n");

    // Add MinimalContext for MCP/browser toolset initialization
    if uses_mcp || uses_browser {
        code.push_str("// Minimal context for toolset initialization\n");
        code.push_str("struct MinimalContext { content: Content }\n");
        code.push_str("impl MinimalContext { fn new() -> Self { Self { content: Content { role: String::new(), parts: vec![] } } } }\n");
        code.push_str("#[async_trait]\n");
        code.push_str("impl ReadonlyContext for MinimalContext {\n");
        code.push_str("    fn invocation_id(&self) -> &str { \"init\" }\n");
        code.push_str("    fn agent_name(&self) -> &str { \"init\" }\n");
        code.push_str("    fn user_id(&self) -> &str { \"init\" }\n");
        code.push_str("    fn app_name(&self) -> &str { \"init\" }\n");
        code.push_str("    fn session_id(&self) -> &str { \"init\" }\n");
        code.push_str("    fn branch(&self) -> &str { \"main\" }\n");
        code.push_str("    fn user_content(&self) -> &Content { &self.content }\n");
        code.push_str("}\n\n");
    }

    // Generate function tools with parameter schemas
    for (agent_id, agent) in &project.agents {
        for tool_type in &agent.tools {
            if tool_type.starts_with("function") {
                let tool_id = format!("{}_{}", agent_id, tool_type);
                if let Some(ToolConfig::Function(config)) = project.tool_configs.get(&tool_id) {
                    code.push_str(&generate_function_schema(config));
                    code.push_str(&generate_function_tool(config));
                }
            }
        }
    }

    code.push_str("#[tokio::main]\n");
    code.push_str("async fn main() -> Result<()> {\n");
    // Initialize tracing with JSON output
    code.push_str("    // Initialize tracing\n");
    code.push_str("    fmt().with_env_filter(EnvFilter::from_default_env().add_directive(\"adk=info\".parse()?)).json().with_writer(std::io::stderr).init();\n\n");
    code.push_str("    let api_key = std::env::var(\"GOOGLE_API_KEY\")\n");
    code.push_str("        .or_else(|_| std::env::var(\"GEMINI_API_KEY\"))\n");
    code.push_str("        .expect(\"GOOGLE_API_KEY or GEMINI_API_KEY must be set\");\n\n");

    // Initialize browser session if any agent uses browser
    let uses_browser = project.agents.values().any(|a| a.tools.contains(&"browser".to_string()));
    if uses_browser {
        code.push_str("    // Initialize browser session\n");
        code.push_str("    let browser_config = BrowserConfig::new().headless(true);\n");
        code.push_str("    let browser = Arc::new(BrowserSession::new(browser_config));\n");
        code.push_str("    browser.start().await?;\n");
        code.push_str("    let browser_toolset = BrowserToolset::new(browser.clone());\n\n");
    }

    // Find top-level agents (not sub-agents of containers)
    let all_sub_agents: std::collections::HashSet<_> =
        project.agents.values().flat_map(|a| a.sub_agents.iter().cloned()).collect();
    let top_level: Vec<_> =
        project.agents.keys().filter(|id| !all_sub_agents.contains(*id)).collect();

    // Build predecessor map from workflow edges (multi-predecessor for fan-in/merge support)
    // This tells us what node(s) come before each node in the workflow.
    //
    // IMPORTANT: We must skip back-edges (edges TO loop nodes that create cycles)
    // to avoid infinite loops when walking the predecessor chain.
    use crate::codegen::action_nodes::ActionNodeConfig;
    use crate::codegen::action_nodes::EvaluationMode;
    let loop_node_ids: std::collections::HashSet<&str> = project
        .action_nodes
        .iter()
        .filter(|(_, n)| matches!(n, ActionNodeConfig::Loop(_)))
        .map(|(id, _)| id.as_str())
        .collect();

    let mut predecessor_map: std::collections::HashMap<&str, Vec<&str>> =
        std::collections::HashMap::new();
    for edge in &project.workflow.edges {
        // Skip trigger nodes - they're entry points, not execution nodes
        let from_is_trigger = project
            .action_nodes
            .get(&edge.from)
            .map(|n| matches!(n, ActionNodeConfig::Trigger(_)))
            .unwrap_or(false);

        // Skip back-edges to loop nodes (these create cycles)
        let is_back_edge = loop_node_ids.contains(edge.to.as_str())
            && edge.from != "START"
            && !edge.from.is_empty()
            && project.action_nodes.get(&edge.from).map(|n| !matches!(n, ActionNodeConfig::Loop(_))).unwrap_or(true)
            && !project.workflow.edges.iter().any(|e2| e2.from == edge.to && e2.to == edge.from);

        if from_is_trigger || is_back_edge {
            continue;
        }

        if edge.from != "START" && edge.to != "END" {
            let preds = predecessor_map.entry(edge.to.as_str()).or_default();
            if !preds.contains(&edge.from.as_str()) {
                preds.push(edge.from.as_str());
            }
        } else if edge.from == "START" {
            let preds = predecessor_map.entry(edge.to.as_str()).or_default();
            if !preds.contains(&"START") {
                preds.push("START");
            }
        }
    }

    // Generate all agent nodes with their predecessors
    for agent_id in &top_level {
        if let Some(agent) = project.agents.get(*agent_id) {
            // Get first predecessor for backward compat (most nodes have exactly one)
            let predecessors = predecessor_map.get(agent_id.as_str());
            let first_predecessor = predecessors.and_then(|v| v.first().copied());
            match agent.agent_type {
                AgentType::Router => {
                    code.push_str(&generate_router_node(agent_id, agent));
                }
                AgentType::Llm => {
                    code.push_str(&generate_llm_node_v2(
                        agent_id,
                        agent,
                        project,
                        first_predecessor,
                        &predecessor_map,
                    ));
                }
                _ => {
                    // Sequential/Loop/Parallel - generate as single node wrapping container
                    code.push_str(&generate_container_node(agent_id, agent, project));
                }
            }
        }
    }

    // Generate action node functions (Set, Transform, etc.) - excluding Trigger which is just an entry point
    let executable_action_nodes: Vec<_> = project
        .action_nodes
        .iter()
        .filter(|(_, node)| {
            !matches!(node, ActionNodeConfig::Trigger(_))
        })
        .collect();

    for (node_id, node) in &executable_action_nodes {
        code.push_str(&generate_action_node_function(node_id, node));
    }

    // Build graph
    code.push_str("    // Build the graph\n");

    // Collect additional state channels needed by action nodes
    // Every key that an action node writes to must be registered as a channel
    let mut extra_channels: Vec<String> = Vec::new();
    for (node_id, node) in &project.action_nodes {
        match node {
            ActionNodeConfig::Set(config) => {
                // Set nodes produce one channel per variable
                for var in &config.variables {
                    if !extra_channels.contains(&var.key) {
                        extra_channels.push(var.key.clone());
                    }
                }
            }
            ActionNodeConfig::Transform(config) => {
                // Transform nodes produce a single output channel
                let key = &config.standard.mapping.output_key;
                if !key.is_empty() && !extra_channels.contains(key) {
                    extra_channels.push(key.clone());
                }
            }
            ActionNodeConfig::Switch(config) => {
                // Switch nodes produce a branch key channel
                let key = if config.standard.mapping.output_key.is_empty() {
                    "branch".to_string()
                } else {
                    config.standard.mapping.output_key.clone()
                };
                if !extra_channels.contains(&key) {
                    extra_channels.push(key);
                }
            }
            ActionNodeConfig::Loop(config) => {
                extra_channels.push(format!("{}_loop_index", node_id));
                extra_channels.push(format!("{}_loop_done", node_id));
                if let Some(fe) = &config.for_each {
                    if !extra_channels.contains(&fe.item_var) {
                        extra_channels.push(fe.item_var.clone());
                    }
                    if !extra_channels.contains(&fe.index_var) {
                        extra_channels.push(fe.index_var.clone());
                    }
                }
                if config.results.collect {
                    let agg_key = config.results.aggregation_key.as_deref().unwrap_or("loop_results");
                    if !extra_channels.contains(&agg_key.to_string()) {
                        extra_channels.push(agg_key.to_string());
                    }
                }
            }
            ActionNodeConfig::Merge(config) => {
                let key = if config.standard.mapping.output_key.is_empty() {
                    "merged".to_string()
                } else {
                    config.standard.mapping.output_key.clone()
                };
                if !extra_channels.contains(&key) {
                    extra_channels.push(key);
                }
                // Also add branch keys as channels (they're read from state)
                if let Some(keys) = &config.branch_keys {
                    for k in keys {
                        if !extra_channels.contains(k) {
                            extra_channels.push(k.clone());
                        }
                    }
                }
            }
            _ => {
                // Other action nodes: register their output_key if set
                let key = &node.standard().mapping.output_key;
                if !key.is_empty() && !extra_channels.contains(key) {
                    extra_channels.push(key.clone());
                }
            }
        }
    }
    let base_channels = vec!["message".to_string(), "classification".to_string(), "response".to_string()];
    let all_channels: Vec<String> = base_channels.into_iter().chain(extra_channels.into_iter()).collect();
    let channel_list = all_channels.iter().map(|c| format!("\"{}\"", c)).collect::<Vec<_>>().join(", ");
    code.push_str(&format!("    let graph = StateGraph::with_channels(&[{}])\n", channel_list));

    // Add all agent nodes
    for agent_id in &top_level {
        code.push_str(&format!("        .add_node({}_node)\n", agent_id));
    }

    // Add action nodes (Set, Transform, etc.) - excluding Trigger
    for (node_id, _) in &executable_action_nodes {
        code.push_str(&format!("        .add_node({}_node)\n", node_id));
    }

    // Add edges from workflow
    // Now we properly include action nodes in the graph execution
    // First, find what START connects to (may be a trigger that we need to skip)
    let start_target =
        project.workflow.edges.iter().find(|e| e.from == "START").map(|e| e.to.as_str());

    // Check if START connects to a trigger - if so, find what the trigger connects to
    let actual_start_target = if let Some(target) = start_target {
        if project
            .action_nodes
            .get(target)
            .map(|n| matches!(n, ActionNodeConfig::Trigger(_)))
            .unwrap_or(false)
        {
            // Find what the trigger connects to
            project.workflow.edges.iter().find(|e| e.from == target).map(|e| e.to.as_str())
        } else {
            Some(target)
        }
    } else {
        None
    };

    // Collect switch node IDs so we can handle their edges specially
    // Separate first_match (conditional routing) from all_match (fan-out)
    let switch_nodes: std::collections::HashSet<&str> = project
        .action_nodes
        .iter()
        .filter(|(_, n)| matches!(n, ActionNodeConfig::Switch(_)))
        .map(|(id, _)| id.as_str())
        .collect();

    let all_match_switch_nodes: std::collections::HashSet<&str> = project
        .action_nodes
        .iter()
        .filter(|(_, n)| {
            if let ActionNodeConfig::Switch(config) = n {
                config.evaluation_mode == EvaluationMode::AllMatch
            } else {
                false
            }
        })
        .map(|(id, _)| id.as_str())
        .collect();

    let first_match_switch_nodes: std::collections::HashSet<&str> = switch_nodes
        .iter()
        .filter(|id| !all_match_switch_nodes.contains(*id))
        .copied()
        .collect();

    // Collect loop node IDs so we can handle their edges specially
    let loop_nodes: std::collections::HashSet<&str> = project
        .action_nodes
        .iter()
        .filter(|(_, n)| matches!(n, ActionNodeConfig::Loop(_)))
        .map(|(id, _)| id.as_str())
        .collect();

    // Build a map of first_match switch_node_id -> Vec<(from_port, target_node)> from edges
    // all_match switches use direct edges (fan-out), so they're NOT in this map
    let mut switch_edge_map: std::collections::HashMap<&str, Vec<(String, String)>> =
        std::collections::HashMap::new();
    for edge in &project.workflow.edges {
        if first_match_switch_nodes.contains(edge.from.as_str()) {
            let port = edge.from_port.clone().unwrap_or_default();
            let target = if edge.to == "END" {
                "END".to_string()
            } else {
                edge.to.clone()
            };
            switch_edge_map.entry(edge.from.as_str()).or_default().push((port, target));
        }
    }

    // Build loop edge maps: loop_id -> (body_target, exit_targets)
    // Edges FROM loop nodes go to the loop body; edges TO loop nodes from body create the cycle
    let mut loop_body_map: std::collections::HashMap<&str, Vec<String>> =
        std::collections::HashMap::new();
    let mut loop_back_edges: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    for edge in &project.workflow.edges {
        if loop_nodes.contains(edge.from.as_str()) {
            loop_body_map.entry(edge.from.as_str()).or_default().push(edge.to.clone());
        }
        // Detect back-edges: edges TO a loop node (cycle)
        if loop_nodes.contains(edge.to.as_str()) && edge.from != "START" {
            loop_back_edges.insert((edge.from.clone(), edge.to.clone()));
        }
    }
    // For each loop node, find the exit target: the node that the loop's downstream
    // chain eventually connects to AFTER the loop body. We look at edges from nodes
    // that also have a back-edge to the loop — their OTHER outgoing edges are exit targets.
    let mut loop_exit_map: std::collections::HashMap<&str, Vec<String>> =
        std::collections::HashMap::new();
    for (back_from, back_to) in &loop_back_edges {
        // Find edges from back_from that don't go to the loop node — those are exit targets
        for edge in &project.workflow.edges {
            if edge.from == *back_from && edge.to != *back_to {
                loop_exit_map.entry(back_to.as_str()).or_default().push(edge.to.clone());
            }
        }
    }
    // If no explicit exit targets found, default to END
    for loop_id in &loop_nodes {
        if !loop_exit_map.contains_key(loop_id) {
            // Check if there are edges from the loop's body nodes to non-loop targets
            // If not, the exit goes to END
            loop_exit_map.entry(loop_id).or_default().push("END".to_string());
        }
    }

    for edge in &project.workflow.edges {
        // Skip edges from trigger nodes (they're entry points, not execution nodes)
        let from_is_trigger = project
            .action_nodes
            .get(&edge.from)
            .map(|n| matches!(n, ActionNodeConfig::Trigger(_)))
            .unwrap_or(false);

        // Skip edges to trigger nodes (shouldn't happen)
        let to_is_trigger = project
            .action_nodes
            .get(&edge.to)
            .map(|n| matches!(n, ActionNodeConfig::Trigger(_)))
            .unwrap_or(false);

        if from_is_trigger {
            continue;
        }

        if to_is_trigger && edge.to != "END" {
            continue;
        }

        // Skip individual edges FROM first_match switch nodes - they'll be handled as conditional edges
        // all_match switch nodes use regular direct edges for fan-out
        if first_match_switch_nodes.contains(edge.from.as_str()) {
            continue;
        }

        // Skip individual edges FROM loop nodes - they'll be handled as conditional edges
        if loop_nodes.contains(edge.from.as_str()) {
            continue;
        }

        // Skip back-edges TO loop nodes - they're part of the loop cycle
        if loop_back_edges.contains(&(edge.from.clone(), edge.to.clone())) {
            continue;
        }

        // Handle START edge - connect to actual first node (skipping trigger if present)
        let (from, to) = if edge.from == "START" {
            if let Some(actual_target) = actual_start_target {
                ("START".to_string(), format!("\"{}\"", actual_target))
            } else {
                continue; // No valid target
            }
        } else {
            let from = format!("\"{}\"", edge.from);
            let to = if edge.to == "END" { "END".to_string() } else { format!("\"{}\"", edge.to) };
            (from, to)
        };

        // Check if source is a router - use conditional edges
        if let Some(agent) = project.agents.get(&edge.from) {
            if agent.agent_type == AgentType::Router && !agent.routes.is_empty() {
                // Generate conditional edges for router
                let conditions: Vec<String> = agent
                    .routes
                    .iter()
                    .map(|r| {
                        let target = if r.target == "END" {
                            "END".to_string()
                        } else {
                            format!("\"{}\"", r.target)
                        };
                        format!("(\"{}\", {})", r.condition, target)
                    })
                    .collect();

                code.push_str("        .add_conditional_edges(\n");
                code.push_str(&format!("            \"{}\",\n", edge.from));
                code.push_str("            Router::by_field(\"classification\"),\n");
                code.push_str(&format!("            [{}],\n", conditions.join(", ")));
                code.push_str("        )\n");
                continue;
            }
        }

        code.push_str(&format!("        .add_edge({}, {})\n", from, to));
    }

    // Generate conditional edges for each switch node
    for (switch_id, targets) in &switch_edge_map {
        if let Some(ActionNodeConfig::Switch(config)) = project.action_nodes.get(*switch_id) {
            let output_key = if config.standard.mapping.output_key.is_empty() {
                "branch"
            } else {
                &config.standard.mapping.output_key
            };

            // Build condition -> target mapping from edges
            let conditions: Vec<String> = targets
                .iter()
                .map(|(port, target)| {
                    let target_str = if target == "END" {
                        "END".to_string()
                    } else {
                        format!("\"{}\"", target)
                    };
                    format!("(\"{}\", {})", port, target_str)
                })
                .collect();

            code.push_str(&format!("        // Switch node: {} - conditional routing by '{}'\n", switch_id, output_key));
            code.push_str("        .add_conditional_edges(\n");
            code.push_str(&format!("            \"{}\",\n", switch_id));
            code.push_str(&format!("            Router::by_field(\"{}\"),\n", output_key));
            code.push_str(&format!("            [{}],\n", conditions.join(", ")));
            code.push_str("        )\n");
        }
    }

    // Generate conditional edges for each loop node
    // Loop nodes use a done flag to decide: continue body or exit
    for loop_id in &loop_nodes {
        let done_key = format!("{}_loop_done", loop_id);
        let body_targets = loop_body_map.get(loop_id).cloned().unwrap_or_default();
        let exit_targets = loop_exit_map.get(loop_id).cloned().unwrap_or_else(|| vec!["END".to_string()]);

        if let Some(body_first) = body_targets.first() {
            let exit_first = exit_targets.first().map(|s| s.as_str()).unwrap_or("END");

            let body_target_str = format!("\"{}\"", body_first);
            let exit_target_str = if exit_first == "END" {
                "END".to_string()
            } else {
                format!("\"{}\"", exit_first)
            };

            code.push_str(&format!("        // Loop node: {} - conditional cycle by '{}'\n", loop_id, done_key));
            code.push_str("        .add_conditional_edges(\n");
            code.push_str(&format!("            \"{}\",\n", loop_id));
            code.push_str(&format!("            Router::by_bool(\"{}\", \"exit\", \"body\"),\n", done_key));
            code.push_str(&format!("            [(\"body\", {}), (\"exit\", {})],\n", body_target_str, exit_target_str));
            code.push_str("        )\n");

            // Add back-edge from last body node to loop node (cycle)
            for (back_from, back_to) in &loop_back_edges {
                if back_to.as_str() == *loop_id {
                    code.push_str(&format!("        .add_edge(\"{}\", \"{}\")\n", back_from, loop_id));
                }
            }
        }
    }

    code.push_str("        .compile()?;\n\n");

    // Interactive loop with streaming and conversation memory
    code.push_str("    // Get session ID from args or generate new one\n");
    code.push_str("    let session_id = std::env::args().nth(1).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());\n");
    code.push_str("    println!(\"SESSION:{}\", session_id);\n\n");
    code.push_str("    // Conversation history for memory\n");
    code.push_str("    let mut history: Vec<(String, String)> = Vec::new();\n\n");
    code.push_str("    // Interactive loop\n");
    code.push_str(
        "    println!(\"Graph workflow ready. Type your message (or 'quit' to exit):\");\n",
    );
    code.push_str("    let stdin = std::io::stdin();\n");
    code.push_str("    let mut input = String::new();\n");
    code.push_str("    let mut turn = 0;\n");
    code.push_str("    loop {\n");
    code.push_str("        input.clear();\n");
    code.push_str("        print!(\"> \");\n");
    code.push_str("        use std::io::Write;\n");
    code.push_str("        std::io::stdout().flush()?;\n");
    code.push_str("        stdin.read_line(&mut input)?;\n");
    code.push_str("        let msg = input.trim();\n");
    code.push_str("        if msg.is_empty() || msg == \"quit\" { break; }\n\n");
    code.push_str("        // Build message with conversation history\n");
    code.push_str("        let context = if history.is_empty() {\n");
    code.push_str("            msg.to_string()\n");
    code.push_str("        } else {\n");
    code.push_str("            let hist: String = history.iter().map(|(u, a)| format!(\"User: {}\\nAssistant: {}\\n\", u, a)).collect();\n");
    code.push_str("            format!(\"{}\\nUser: {}\", hist, msg)\n");
    code.push_str("        };\n\n");
    code.push_str("        let mut state = State::new();\n");
    code.push_str("        state.insert(\"message\".to_string(), json!(context));\n");
    code.push_str("        \n");
    code.push_str("        use adk_graph::StreamMode;\n");
    code.push_str("        use tokio_stream::StreamExt;\n");
    code.push_str("        let stream = graph.stream(state, ExecutionConfig::new(&format!(\"{}-turn-{}\", session_id, turn)), StreamMode::Messages);\n");
    code.push_str("        tokio::pin!(stream);\n");
    code.push_str("        let mut final_response = String::new();\n");
    code.push_str("        \n");
    code.push_str("        while let Some(event) = stream.next().await {\n");
    code.push_str("            match event {\n");
    code.push_str("                Ok(e) => {\n");
    code.push_str("                    // Stream Message events as chunks\n");
    code.push_str(
        "                    if let adk_graph::StreamEvent::Message { content, .. } = &e {\n",
    );
    code.push_str("                        final_response.push_str(content);\n");
    code.push_str("                        println!(\"CHUNK:{}\", serde_json::to_string(&final_response).unwrap_or_default());\n");
    code.push_str("                    }\n");
    code.push_str("                    // Output trace event as JSON\n");
    code.push_str("                    if let Ok(json) = serde_json::to_string(&e) {\n");
    code.push_str("                        println!(\"TRACE:{}\", json);\n");
    code.push_str("                    }\n");
    code.push_str("                    // Capture final response from Done event\n");
    code.push_str("                    if let adk_graph::StreamEvent::Done { state, .. } = &e {\n");
    code.push_str("                        if let Some(resp) = state.get(\"response\").and_then(|v| v.as_str()) {\n");
    code.push_str("                            final_response = resp.to_string();\n");
    code.push_str("                        }\n");
    code.push_str("                    }\n");
    code.push_str("                }\n");
    code.push_str("                Err(e) => eprintln!(\"Error: {}\", e),\n");
    code.push_str("            }\n");
    code.push_str("        }\n");
    code.push_str("        turn += 1;\n\n");
    code.push_str("        // Save to history\n");
    code.push_str("        if !final_response.is_empty() {\n");
    code.push_str("            history.push((msg.to_string(), final_response.clone()));\n");
    code.push_str("            println!(\"RESPONSE:{}\", serde_json::to_string(&final_response).unwrap_or_default());\n");
    code.push_str("        }\n");
    code.push_str("    }\n\n");

    code.push_str("    Ok(())\n");
    code.push_str("}\n");

    code
}

fn generate_router_node(id: &str, agent: &AgentSchema) -> String {
    let mut code = String::new();
    let model = agent.model.as_deref().unwrap_or("gemini-2.0-flash");

    code.push_str(&format!("    // Router: {}\n", id));
    code.push_str(&format!("    let {}_llm = Arc::new(\n", id));
    code.push_str(&format!("        LlmAgentBuilder::new(\"{}\")\n", id));
    code.push_str(&format!(
        "            .model(Arc::new(GeminiModel::new(&api_key, \"{}\")?))\n",
        model
    ));

    let route_options: Vec<&str> = agent.routes.iter().map(|r| r.condition.as_str()).collect();
    let instruction = if agent.instruction.is_empty() {
        format!(
            "Classify the input into one of: {}. Respond with ONLY the category name.",
            route_options.join(", ")
        )
    } else {
        agent.instruction.clone()
    };
    let escaped = instruction.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
    code.push_str(&format!("            .instruction(\"{}\")\n", escaped));
    code.push_str("            .build()?\n");
    code.push_str("    );\n\n");

    code.push_str(&format!("    let {}_node = AgentNode::new({}_llm)\n", id, id));
    code.push_str("        .with_input_mapper(|state| {\n");
    code.push_str(
        "            let msg = state.get(\"message\").and_then(|v| v.as_str()).unwrap_or(\"\");\n",
    );
    code.push_str("            adk_core::Content::new(\"user\").with_text(msg.to_string())\n");
    code.push_str("        })\n");
    code.push_str("        .with_output_mapper(|events| {\n");
    code.push_str("            let mut updates = std::collections::HashMap::new();\n");
    code.push_str("            for event in events {\n");
    code.push_str("                if let Some(content) = event.content() {\n");
    code.push_str("                    let text: String = content.parts.iter()\n");
    code.push_str("                        .filter_map(|p| p.text())\n");
    code.push_str("                        .collect::<Vec<_>>().join(\"\").to_lowercase();\n");

    for (i, route) in agent.routes.iter().enumerate() {
        let cond = if i == 0 { "if" } else { "else if" };
        code.push_str(&format!(
            "                    {} text.contains(\"{}\") {{\n",
            cond,
            route.condition.to_lowercase()
        ));
        code.push_str(&format!("                        updates.insert(\"classification\".to_string(), json!(\"{}\"));\n", route.condition));
        code.push_str("                    }\n");
    }
    if let Some(first) = agent.routes.first() {
        code.push_str(&format!("                    else {{ updates.insert(\"classification\".to_string(), json!(\"{}\")); }}\n", first.condition));
    }

    code.push_str("                }\n");
    code.push_str("            }\n");
    code.push_str("            updates\n");
    code.push_str("        });\n\n");

    code
}

/// Generate LLM node with predecessor-based input mapping
///
/// This version uses the workflow edges to determine what each agent reads from:
/// - If predecessor is START or a trigger: read from "message"
/// - If predecessor is another agent: read from "response"
/// - If predecessor is an action node (Set, Transform): read from "response" (action nodes pass through)
fn generate_llm_node_v2(
    id: &str,
    agent: &AgentSchema,
    project: &ProjectSchema,
    predecessor: Option<&str>,
    predecessor_map: &std::collections::HashMap<&str, Vec<&str>>,
) -> String {
    let mut code = String::new();
    let model = agent.model.as_deref().unwrap_or("gemini-2.0-flash");

    code.push_str(&format!("    // Agent: {}\n", id));

    // Generate MCP toolsets for all MCP tools (mcp, mcp_1, mcp_2, etc.)
    let mcp_tools: Vec<_> =
        agent.tools.iter().filter(|t| *t == "mcp" || t.starts_with("mcp_")).collect();

    for (idx, mcp_tool) in mcp_tools.iter().enumerate() {
        let tool_id = format!("{}_{}", id, mcp_tool);
        if let Some(ToolConfig::Mcp(config)) = project.tool_configs.get(&tool_id) {
            let cmd = &config.server_command;
            let var_suffix = if idx == 0 { "mcp".to_string() } else { format!("mcp_{}", idx + 1) };
            code.push_str(&format!(
                "    let mut {}_{}_cmd = Command::new(\"{}\");\n",
                id, var_suffix, cmd
            ));
            for arg in &config.server_args {
                code.push_str(&format!("    {}_{}_cmd.arg(\"{}\");\n", id, var_suffix, arg));
            }
            code.push_str(&format!(
                "    let {}_{}_client = tokio::time::timeout(\n",
                id, var_suffix
            ));
            code.push_str("        std::time::Duration::from_secs(10),\n");
            code.push_str(&format!(
                "        ().serve(TokioChildProcess::new({}_{}_cmd)?)\n",
                id, var_suffix
            ));
            code.push_str(&format!("    ).await.map_err(|_| anyhow::anyhow!(\"MCP server '{}' failed to start within 10s\"))??;\n", cmd));
            code.push_str(&format!(
                "    let {}_{}_toolset = McpToolset::new({}_{}_client)",
                id, var_suffix, id, var_suffix
            ));
            if !config.tool_filter.is_empty() {
                code.push_str(&format!(
                    ".with_tools(&[{}])",
                    config
                        .tool_filter
                        .iter()
                        .map(|t| format!("\"{}\"", t))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            code.push_str(";\n");
            code.push_str(&format!("    let {}_{}_tools = {}_{}_toolset.tools(Arc::new(MinimalContext::new())).await?;\n", id, var_suffix, id, var_suffix));
            code.push_str(&format!(
                "    eprintln!(\"Loaded {{}} tools from MCP server '{}'\", {}_{}_tools.len());\n\n",
                cmd, id, var_suffix
            ));
        }
    }

    code.push_str(&format!("    let mut {}_builder = LlmAgentBuilder::new(\"{}\")\n", id, id));
    code.push_str(&format!(
        "        .model(Arc::new(GeminiModel::new(&api_key, \"{}\")?));\n",
        model
    ));

    if !agent.instruction.is_empty() {
        // Strip {{var}} template variables from instruction - they'll be injected via input_mapper
        // This avoids the session state vs graph state mismatch where the agent's template
        // system looks at session state but Set nodes update graph state
        let instruction_clean = strip_template_variables(&agent.instruction);
        let escaped =
            instruction_clean.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
        code.push_str(&format!(
            "    {}_builder = {}_builder.instruction(\"{}\");\n",
            id, id, escaped
        ));
    }

    // Add MCP tools if present
    for (idx, mcp_tool) in mcp_tools.iter().enumerate() {
        let tool_id = format!("{}_{}", id, mcp_tool);
        if project.tool_configs.contains_key(&tool_id) {
            let var_suffix = if idx == 0 { "mcp".to_string() } else { format!("mcp_{}", idx + 1) };
            code.push_str(&format!("    for tool in {}_{}_tools {{\n", id, var_suffix));
            code.push_str(&format!("        {}_builder = {}_builder.tool(tool);\n", id, id));
            code.push_str("    }\n");
        }
    }

    for tool_type in &agent.tools {
        if tool_type.starts_with("function") {
            let tool_id = format!("{}_{}", id, tool_type);
            if let Some(ToolConfig::Function(config)) = project.tool_configs.get(&tool_id) {
                let struct_name = to_pascal_case(&config.name);
                code.push_str(&format!("    {}_builder = {}_builder.tool(Arc::new(FunctionTool::new(\"{}\", \"{}\", {}_fn).with_parameters_schema::<{}Args>()));\n", 
                    id, id, config.name, config.description.replace('"', "\\\""), config.name, struct_name));
            }
        } else if !tool_type.starts_with("mcp") {
            match tool_type.as_str() {
                "google_search" => code.push_str(&format!(
                    "    {}_builder = {}_builder.tool(Arc::new(GoogleSearchTool::new()));\n",
                    id, id
                )),
                "exit_loop" => code.push_str(&format!(
                    "    {}_builder = {}_builder.tool(Arc::new(ExitLoopTool::new()));\n",
                    id, id
                )),
                "load_artifact" => code.push_str(&format!(
                    "    {}_builder = {}_builder.tool(Arc::new(LoadArtifactsTool::new()));\n",
                    id, id
                )),
                "browser" => {
                    code.push_str("    for tool in browser_toolset.tools(Arc::new(MinimalContext::new())).await? {\n");
                    code.push_str(&format!(
                        "        {}_builder = {}_builder.tool(tool);\n",
                        id, id
                    ));
                    code.push_str("    }\n");
                }
                _ => {}
            }
        }
    }

    code.push_str(&format!("    let {}_llm = Arc::new({}_builder.build()?);\n\n", id, id));

    code.push_str(&format!("    let {}_node = AgentNode::new({}_llm)\n", id, id));
    code.push_str("        .with_input_mapper(|state| {\n");

    // Determine what to read based on predecessor
    let is_first = predecessor == Some("START") || predecessor.is_none();

    if is_first {
        code.push_str("            // First node: read from original message\n");
        code.push_str("            let msg = state.get(\"message\").and_then(|v| v.as_str()).unwrap_or(\"\");\n");
    } else {
        code.push_str(&format!(
            "            // Predecessor: {} - read from response\n",
            predecessor.unwrap_or("unknown")
        ));
        code.push_str("            let msg = state.get(\"response\").and_then(|v| v.as_str())\n");
        code.push_str("                .or_else(|| state.get(\"message\").and_then(|v| v.as_str())).unwrap_or(\"\");\n");
    }

    // Collect state variables to inject into the agent's input:
    // 1. Variables from {{var}} references in the instruction
    // 2. Variables from predecessor Set/Transform action nodes
    let mut inject_vars: Vec<String> = Vec::new();

    // Check if instruction references state variables via {{var}} syntax
    let var_refs: Vec<&str> = agent
        .instruction
        .match_indices("{{")
        .filter_map(|(start, _)| {
            let rest = &agent.instruction[start + 2..];
            rest.find("}}").map(|end| &rest[..end])
        })
        .collect();
    for var in &var_refs {
        if *var != "message" && *var != "response" && !inject_vars.contains(&var.to_string()) {
            inject_vars.push(var.to_string());
        }
    }

    // Check if any predecessor in the chain is a Set or Transform action node
    // Walk backwards through the predecessor chain to find all Set nodes that feed into this agent
    // Supports multiple predecessors (fan-in from merge/parallel branches)
    {
        use crate::codegen::action_nodes::ActionNodeConfig;
        let mut queue: Vec<&str> = if let Some(pred) = predecessor {
            vec![pred]
        } else {
            vec![]
        };
        // Also add any additional predecessors (fan-in)
        if let Some(preds) = predecessor_map.get(id) {
            for p in preds {
                if !queue.contains(p) {
                    queue.push(p);
                }
            }
        }
        let mut visited_preds = std::collections::HashSet::new();
        while let Some(pred_id) = queue.pop() {
            if pred_id == "START" || visited_preds.contains(pred_id) {
                continue;
            }
            visited_preds.insert(pred_id);
            if let Some(action_node) = project.action_nodes.get(pred_id) {
                match action_node {
                    ActionNodeConfig::Set(set_config) => {
                        for var in &set_config.variables {
                            if !inject_vars.contains(&var.key) {
                                inject_vars.push(var.key.clone());
                            }
                        }
                    }
                    ActionNodeConfig::Transform(transform_config) => {
                        let out_key = &transform_config.standard.mapping.output_key;
                        if !inject_vars.contains(out_key) {
                            inject_vars.push(out_key.clone());
                        }
                    }
                    ActionNodeConfig::Merge(merge_config) => {
                        // Merge node output key contains the combined result
                        let out_key = if merge_config.standard.mapping.output_key.is_empty() {
                            "merged".to_string()
                        } else {
                            merge_config.standard.mapping.output_key.clone()
                        };
                        if !inject_vars.contains(&out_key) {
                            inject_vars.push(out_key);
                        }
                    }
                    _ => {}
                }
            }
            // Walk to all predecessors of this node
            if let Some(preds) = predecessor_map.get(pred_id) {
                for p in preds {
                    if !visited_preds.contains(*p) {
                        queue.push(p);
                    }
                }
            }
        }
    }

    if !inject_vars.is_empty() {
        code.push_str(
            "            // Include state variables from Set nodes and instruction references\n",
        );
        code.push_str("            let mut full_msg = msg.to_string();\n");
        code.push_str("            let mut context_parts: Vec<String> = Vec::new();\n");
        for var in &inject_vars {
            code.push_str(&format!("            if let Some(v) = state.get(\"{}\") {{\n", var));
            code.push_str(&format!("                context_parts.push(format!(\"{}: {{}}\", v.as_str().unwrap_or(&v.to_string())));\n", var));
            code.push_str("            }\n");
        }
        code.push_str("            if !context_parts.is_empty() {\n");
        code.push_str("                full_msg = format!(\"{}\\n\\nContext:\\n{}\", full_msg, context_parts.join(\"\\n\"));\n");
        code.push_str("            }\n");
        code.push_str("            adk_core::Content::new(\"user\").with_text(full_msg)\n");
    } else {
        code.push_str("            adk_core::Content::new(\"user\").with_text(msg.to_string())\n");
    }
    code.push_str("        })\n");
    code.push_str("        .with_output_mapper(|events| {\n");
    code.push_str("            let mut updates = std::collections::HashMap::new();\n");
    code.push_str("            let mut full_text = String::new();\n");
    code.push_str("            for event in events {\n");
    code.push_str("                if let Some(content) = event.content() {\n");
    code.push_str("                    for part in &content.parts {\n");
    code.push_str("                        if let Some(text) = part.text() {\n");
    code.push_str("                            full_text.push_str(text);\n");
    code.push_str("                        }\n");
    code.push_str("                    }\n");
    code.push_str("                }\n");
    code.push_str("            }\n");
    code.push_str("            if !full_text.is_empty() {\n");
    code.push_str("                updates.insert(\"response\".to_string(), json!(full_text));\n");
    code.push_str("            }\n");
    code.push_str("            updates\n");
    code.push_str("        });\n\n");

    code
}

fn generate_container_node(id: &str, agent: &AgentSchema, project: &ProjectSchema) -> String {
    let mut code = String::new();

    // Generate sub-agents first
    for sub_id in &agent.sub_agents {
        if let Some(sub) = project.agents.get(sub_id) {
            let model = sub.model.as_deref().unwrap_or("gemini-2.0-flash");
            let has_tools = !sub.tools.is_empty();
            let has_instruction = !sub.instruction.is_empty();
            let mut_kw = if has_tools || has_instruction { "mut " } else { "" };

            // Load MCP tools BEFORE creating builder (matching working pattern)
            for tool_type in &sub.tools {
                let tool_id = format!("{}_{}", sub_id, tool_type);
                if tool_type.starts_with("mcp") {
                    if let Some(ToolConfig::Mcp(config)) = project.tool_configs.get(&tool_id) {
                        let var_suffix = tool_type.replace("mcp_", "mcp");
                        code.push_str(&format!(
                            "    let mut {}_{}_cmd = Command::new(\"{}\");\n",
                            sub_id, var_suffix, config.server_command
                        ));
                        for arg in &config.server_args {
                            code.push_str(&format!(
                                "    {}_{}_cmd.arg(\"{}\");\n",
                                sub_id, var_suffix, arg
                            ));
                        }
                        code.push_str(&format!(
                            "    let {}_{}_client = tokio::time::timeout(\n",
                            sub_id, var_suffix
                        ));
                        code.push_str("        std::time::Duration::from_secs(10),\n");
                        code.push_str(&format!(
                            "        ().serve(TokioChildProcess::new({}_{}_cmd)?)\n",
                            sub_id, var_suffix
                        ));
                        code.push_str(&format!("    ).await.map_err(|_| anyhow::anyhow!(\"MCP server '{}' failed to start within 10s\"))??;\n", config.server_command));
                        code.push_str(&format!(
                            "    let {}_{}_toolset = McpToolset::new({}_{}_client);\n",
                            sub_id, var_suffix, sub_id, var_suffix
                        ));
                        code.push_str(&format!("    let {}_{}_tools = {}_{}_toolset.tools(Arc::new(MinimalContext::new())).await?;\n", sub_id, var_suffix, sub_id, var_suffix));
                        code.push_str(&format!("    eprintln!(\"Loaded {{}} tools from MCP server '{}'\", {}_{}_tools.len());\n\n", config.server_command, sub_id, var_suffix));
                    }
                }
            }

            // Create builder
            code.push_str(&format!(
                "    let {}{}_builder = LlmAgentBuilder::new(\"{}\")\n",
                mut_kw, sub_id, sub_id
            ));
            code.push_str(&format!(
                "        .model(Arc::new(GeminiModel::new(&api_key, \"{}\")?))",
                model
            ));
            code.push_str(";\n");

            // Add instruction separately (matching working pattern)
            if !sub.instruction.is_empty() {
                let escaped =
                    sub.instruction.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
                code.push_str(&format!(
                    "    {}_builder = {}_builder.instruction(\"{}\");\n",
                    sub_id, sub_id, escaped
                ));
            }

            // Add tools
            for tool_type in &sub.tools {
                let tool_id = format!("{}_{}", sub_id, tool_type);
                if tool_type.starts_with("function") {
                    if let Some(ToolConfig::Function(config)) = project.tool_configs.get(&tool_id) {
                        let fn_name = &config.name;
                        let struct_name = to_pascal_case(fn_name);
                        code.push_str(&format!("    {}_builder = {}_builder.tool(Arc::new(FunctionTool::new(\"{}\", \"{}\", {}_fn).with_parameters_schema::<{}Args>()));\n", 
                            sub_id, sub_id, fn_name, config.description.replace('"', "\\\""), fn_name, struct_name));
                    }
                } else if tool_type.starts_with("mcp") {
                    // Only generate tool loop if config exists (MCP setup was generated above)
                    let tool_id = format!("{}_{}", sub_id, tool_type);
                    if project.tool_configs.contains_key(&tool_id) {
                        let var_suffix = tool_type.replace("mcp_", "mcp");
                        code.push_str(&format!(
                            "    for tool in {}_{}_tools {{\n",
                            sub_id, var_suffix
                        ));
                        code.push_str(&format!(
                            "        {}_builder = {}_builder.tool(tool);\n",
                            sub_id, sub_id
                        ));
                        code.push_str("    }\n");
                    }
                } else if tool_type == "google_search" {
                    code.push_str(&format!(
                        "    {}_builder = {}_builder.tool(Arc::new(GoogleSearchTool::new()));\n",
                        sub_id, sub_id
                    ));
                } else if tool_type == "exit_loop" {
                    code.push_str(&format!(
                        "    {}_builder = {}_builder.tool(Arc::new(ExitLoopTool::new()));\n",
                        sub_id, sub_id
                    ));
                } else if tool_type == "load_artifact" {
                    code.push_str(&format!(
                        "    {}_builder = {}_builder.tool(Arc::new(LoadArtifactsTool::new()));\n",
                        sub_id, sub_id
                    ));
                }
            }

            code.push_str(&format!("    let {}_agent = {}_builder.build()?;\n\n", sub_id, sub_id));
        }
    }

    // Create container
    let subs: Vec<_> = agent.sub_agents.iter().map(|s| format!("Arc::new({}_agent)", s)).collect();
    let container_type = match agent.agent_type {
        AgentType::Sequential => "adk_agent::SequentialAgent",
        AgentType::Loop => "adk_agent::LoopAgent",
        AgentType::Parallel => "adk_agent::ParallelAgent",
        _ => "adk_agent::SequentialAgent",
    };

    code.push_str(&format!("    // Container: {} ({:?})\n", id, agent.agent_type));
    if agent.agent_type == AgentType::Loop {
        let max_iter = agent.max_iterations.unwrap_or(3);
        code.push_str(&format!(
            "    let {}_container = {}::new(\"{}\", vec![{}]).with_max_iterations({});\n\n",
            id,
            container_type,
            id,
            subs.join(", "),
            max_iter
        ));
    } else {
        code.push_str(&format!(
            "    let {}_container = {}::new(\"{}\", vec![{}]);\n\n",
            id,
            container_type,
            id,
            subs.join(", ")
        ));
    }

    // Wrap in AgentNode
    code.push_str(&format!("    let {}_node = AgentNode::new(Arc::new({}_container))\n", id, id));
    code.push_str("        .with_input_mapper(|state| {\n");
    code.push_str(
        "            let msg = state.get(\"message\").and_then(|v| v.as_str()).unwrap_or(\"\");\n",
    );
    code.push_str("            adk_core::Content::new(\"user\").with_text(msg.to_string())\n");
    code.push_str("        })\n");
    code.push_str("        .with_output_mapper(|events| {\n");
    code.push_str("            let mut updates = std::collections::HashMap::new();\n");
    code.push_str("            let mut full_text = String::new();\n");
    code.push_str("            for event in events {\n");
    code.push_str("                if let Some(content) = event.content() {\n");
    code.push_str("                    for part in &content.parts {\n");
    code.push_str("                        if let Some(text) = part.text() {\n");
    code.push_str("                            full_text.push_str(text);\n");
    code.push_str("                        }\n");
    code.push_str("                    }\n");
    code.push_str("                }\n");
    code.push_str("            }\n");
    code.push_str("            // Filter out tool call artifacts\n");
    code.push_str("            let full_text = full_text.replace(\"exit_loop\", \"\");\n");
    code.push_str("            if !full_text.is_empty() {\n");
    code.push_str("                updates.insert(\"response\".to_string(), json!(full_text));\n");
    code.push_str("            }\n");
    code.push_str("            updates\n");
    code.push_str("        });\n\n");

    code
}

fn generate_function_tool(config: &crate::schema::FunctionToolConfig) -> String {
    let mut code = String::new();
    let fn_name = &config.name;

    code.push_str(&format!("async fn {}_fn(_ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value, adk_core::AdkError> {{\n", fn_name));

    // Generate parameter extraction
    for param in &config.parameters {
        let extract = match param.param_type {
            crate::schema::ParamType::String => format!(
                "    let {} = args[\"{}\"].as_str().unwrap_or(\"\");\n",
                param.name, param.name
            ),
            crate::schema::ParamType::Number => format!(
                "    let {} = args[\"{}\"].as_f64().unwrap_or(0.0);\n",
                param.name, param.name
            ),
            crate::schema::ParamType::Boolean => format!(
                "    let {} = args[\"{}\"].as_bool().unwrap_or(false);\n",
                param.name, param.name
            ),
        };
        code.push_str(&extract);
    }

    code.push('\n');

    // Insert user's code or generate placeholder
    if config.code.is_empty() {
        // Generate placeholder that echoes parameters
        let param_json = config
            .parameters
            .iter()
            .map(|p| format!("        \"{}\": {}", p.name, p.name))
            .collect::<Vec<_>>()
            .join(",\n");
        code.push_str("    // TODO: Add function implementation\n");
        code.push_str("    Ok(json!({\n");
        code.push_str(&format!("        \"function\": \"{}\",\n", fn_name));
        if !param_json.is_empty() {
            code.push_str(&param_json);
            code.push_str(",\n");
        }
        code.push_str("        \"status\": \"not_implemented\"\n");
        code.push_str("    }))\n");
    } else {
        // Use user's actual code
        code.push_str("    // User-defined implementation\n");
        for line in config.code.lines() {
            code.push_str(&format!("    {}\n", line));
        }
    }

    code.push_str("}\n\n");
    code
}

fn generate_function_schema(config: &crate::schema::FunctionToolConfig) -> String {
    let mut code = String::new();
    let struct_name = to_pascal_case(&config.name);

    code.push_str("#[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]\n");
    code.push_str(&format!("struct {}Args {{\n", struct_name));

    for param in &config.parameters {
        if !param.description.is_empty() {
            code.push_str(&format!("    /// {}\n", param.description));
        }
        let rust_type = match param.param_type {
            crate::schema::ParamType::String => "String",
            crate::schema::ParamType::Number => "f64",
            crate::schema::ParamType::Boolean => "bool",
        };
        code.push_str(&format!("    {}: {},\n", param.name, rust_type));
    }

    code.push_str("}\n\n");
    code
}

fn to_pascal_case(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().chain(chars).collect(),
            }
        })
        .collect()
}

/// Generate a graph node function for an action node (Set, Transform, etc.)
/// Helper: generate a condition check for first_match Switch (sets matched_branch)
fn generate_condition_check(code: &mut String, op: &str, value_str: &str, port: &str, field: &str) {
    let escaped_val = value_str.replace('"', "\\\"");
    match op {
        "equals" | "==" | "eq" => {
            code.push_str(&format!(
                "            if field_val == \"{}\" {{ matched_branch = Some(\"{}\"); }}\n",
                escaped_val, port
            ));
        }
        "not_equals" | "!=" | "neq" => {
            code.push_str(&format!(
                "            if field_val != \"{}\" {{ matched_branch = Some(\"{}\"); }}\n",
                escaped_val, port
            ));
        }
        "contains" => {
            code.push_str(&format!(
                "            if field_val.contains(\"{}\") {{ matched_branch = Some(\"{}\"); }}\n",
                escaped_val, port
            ));
        }
        "greater_than" | ">" | "gt" => {
            code.push_str(&format!(
                "            if field_val > \"{}\" {{ matched_branch = Some(\"{}\"); }}\n",
                escaped_val, port
            ));
        }
        "less_than" | "<" | "lt" => {
            code.push_str(&format!(
                "            if field_val < \"{}\" {{ matched_branch = Some(\"{}\"); }}\n",
                escaped_val, port
            ));
        }
        "gte" | ">=" => {
            code.push_str(&format!(
                "            if field_val >= \"{}\" {{ matched_branch = Some(\"{}\"); }}\n",
                escaped_val, port
            ));
        }
        "lte" | "<=" => {
            code.push_str(&format!(
                "            if field_val <= \"{}\" {{ matched_branch = Some(\"{}\"); }}\n",
                escaped_val, port
            ));
        }
        "startsWith" => {
            code.push_str(&format!(
                "            if field_val.starts_with(\"{}\") {{ matched_branch = Some(\"{}\"); }}\n",
                escaped_val, port
            ));
        }
        "endsWith" => {
            code.push_str(&format!(
                "            if field_val.ends_with(\"{}\") {{ matched_branch = Some(\"{}\"); }}\n",
                escaped_val, port
            ));
        }
        "empty" => {
            code.push_str(&format!(
                "            if field_val.is_empty() {{ matched_branch = Some(\"{}\"); }}\n",
                port
            ));
        }
        "exists" => {
            code.push_str(&format!(
                "            if ctx.state.contains_key(\"{}\") {{ matched_branch = Some(\"{}\"); }}\n",
                field, port
            ));
        }
        _ => {
            code.push_str(&format!(
                "            if field_val == \"{}\" {{ matched_branch = Some(\"{}\"); }}\n",
                escaped_val, port
            ));
        }
    }
}

/// Helper: generate a condition check for all_match Switch (pushes to matched_branches vec)
fn generate_all_match_condition_check(code: &mut String, op: &str, value_str: &str, port: &str, field: &str) {
    let escaped_val = value_str.replace('"', "\\\"");
    match op {
        "equals" | "==" | "eq" => {
            code.push_str(&format!(
                "            if field_val == \"{}\" {{ matched_branches.push(\"{}\".to_string()); }}\n",
                escaped_val, port
            ));
        }
        "not_equals" | "!=" | "neq" => {
            code.push_str(&format!(
                "            if field_val != \"{}\" {{ matched_branches.push(\"{}\".to_string()); }}\n",
                escaped_val, port
            ));
        }
        "contains" => {
            code.push_str(&format!(
                "            if field_val.contains(\"{}\") {{ matched_branches.push(\"{}\".to_string()); }}\n",
                escaped_val, port
            ));
        }
        "greater_than" | ">" | "gt" => {
            code.push_str(&format!(
                "            if field_val > \"{}\" {{ matched_branches.push(\"{}\".to_string()); }}\n",
                escaped_val, port
            ));
        }
        "less_than" | "<" | "lt" => {
            code.push_str(&format!(
                "            if field_val < \"{}\" {{ matched_branches.push(\"{}\".to_string()); }}\n",
                escaped_val, port
            ));
        }
        "gte" | ">=" => {
            code.push_str(&format!(
                "            if field_val >= \"{}\" {{ matched_branches.push(\"{}\".to_string()); }}\n",
                escaped_val, port
            ));
        }
        "lte" | "<=" => {
            code.push_str(&format!(
                "            if field_val <= \"{}\" {{ matched_branches.push(\"{}\".to_string()); }}\n",
                escaped_val, port
            ));
        }
        "startsWith" => {
            code.push_str(&format!(
                "            if field_val.starts_with(\"{}\") {{ matched_branches.push(\"{}\".to_string()); }}\n",
                escaped_val, port
            ));
        }
        "endsWith" => {
            code.push_str(&format!(
                "            if field_val.ends_with(\"{}\") {{ matched_branches.push(\"{}\".to_string()); }}\n",
                escaped_val, port
            ));
        }
        "empty" => {
            code.push_str(&format!(
                "            if field_val.is_empty() {{ matched_branches.push(\"{}\".to_string()); }}\n",
                port
            ));
        }
        "exists" => {
            code.push_str(&format!(
                "            if ctx.state.contains_key(\"{}\") {{ matched_branches.push(\"{}\".to_string()); }}\n",
                field, port
            ));
        }
        _ => {
            code.push_str(&format!(
                "            if field_val == \"{}\" {{ matched_branches.push(\"{}\".to_string()); }}\n",
                escaped_val, port
            ));
        }
    }
}

fn generate_action_node_function(
    node_id: &str,
    node: &crate::codegen::action_nodes::ActionNodeConfig,
) -> String {
    use crate::codegen::action_nodes::ActionNodeConfig;

    let mut code = String::new();

    match node {
        ActionNodeConfig::Set(config) => {
            code.push_str(&format!("    // Action Node: {} (Set)\n", config.standard.name));
            code.push_str(&format!("    let {}_node = adk_graph::node::FunctionNode::new(\"{}\", |ctx| async move {{\n", node_id, node_id));
            code.push_str("        let mut output = NodeOutput::new();\n");

            // Generate variable setting logic
            for var in &config.variables {
                let key = &var.key;
                match var.value_type.as_str() {
                    "expression" => {
                        // Expression type - interpolate variables from state
                        let expr = var.value.as_str().unwrap_or("");
                        // Simple variable interpolation: replace {{var}} with state value
                        code.push_str(&format!("        // Set {} from expression\n", key));
                        code.push_str(&format!(
                            "        let mut {}_value = \"{}\".to_string();\n",
                            key,
                            expr.replace('"', "\\\"")
                        ));
                        code.push_str("        // Interpolate variables from state\n");
                        code.push_str("        for (k, v) in ctx.state.iter() {\n");
                        code.push_str("            let pattern = format!(\"{{{{{}}}}}\", k);\n");
                        code.push_str("            if let Some(s) = v.as_str() {\n");
                        code.push_str(&format!(
                            "                {}_value = {}_value.replace(&pattern, s);\n",
                            key, key
                        ));
                        code.push_str("            } else {\n");
                        code.push_str(&format!("                {}_value = {}_value.replace(&pattern, &v.to_string());\n", key, key));
                        code.push_str("            }\n");
                        code.push_str("        }\n");
                        code.push_str(&format!(
                            "        output = output.with_update(\"{}\", json!({}_value));\n",
                            key, key
                        ));
                    }
                    "json" => {
                        // JSON type - use value directly
                        code.push_str(&format!(
                            "        output = output.with_update(\"{}\", json!({}));\n",
                            key, var.value
                        ));
                    }
                    _ => {
                        // String or other types
                        code.push_str(&format!(
                            "        output = output.with_update(\"{}\", json!({}));\n",
                            key, var.value
                        ));
                    }
                }
            }

            code.push_str("        Ok(output)\n");
            code.push_str("    });\n\n");
        }
        ActionNodeConfig::Transform(config) => {
            code.push_str(&format!("    // Action Node: {} (Transform)\n", config.standard.name));
            code.push_str(&format!("    let {}_node = adk_graph::node::FunctionNode::new(\"{}\", |ctx| async move {{\n", node_id, node_id));

            // Simple template transformation
            let expr = &config.expression;
            code.push_str(&format!(
                "        let mut result = \"{}\".to_string();\n",
                expr.replace('"', "\\\"")
            ));
            code.push_str("        for (k, v) in ctx.state.iter() {\n");
            code.push_str("            let pattern = format!(\"{{{{{}}}}}\", k);\n");
            code.push_str("            if let Some(s) = v.as_str() {\n");
            code.push_str("                result = result.replace(&pattern, s);\n");
            code.push_str("            } else {\n");
            code.push_str("                result = result.replace(&pattern, &v.to_string());\n");
            code.push_str("            }\n");
            code.push_str("        }\n");

            let output_key = &config.standard.mapping.output_key;
            code.push_str(&format!(
                "        Ok(NodeOutput::new().with_update(\"{}\", json!(result)))\n",
                output_key
            ));
            code.push_str("    });\n\n");
        }
        ActionNodeConfig::Switch(config) => {
            code.push_str(&format!("    // Action Node: {} (Switch)\n", config.standard.name));
            code.push_str(&format!("    let {}_node = adk_graph::node::FunctionNode::new(\"{}\", |ctx| async move {{\n", node_id, node_id));

            let output_key = if config.standard.mapping.output_key.is_empty() {
                "branch"
            } else {
                &config.standard.mapping.output_key
            };

            match config.evaluation_mode {
                crate::codegen::action_nodes::EvaluationMode::FirstMatch => {
                    // First match: evaluate conditions, return first matching branch for routing
                    code.push_str("        let mut matched_branch: Option<&str> = None;\n");

                    for condition in &config.conditions {
                        let field = &condition.field;
                        let op = &condition.operator;
                        let port = &condition.output_port;
                        let value_str = condition
                            .value
                            .as_ref()
                            .map(|v| match v {
                                serde_json::Value::String(s) => s.clone(),
                                other => other.to_string(),
                            })
                            .unwrap_or_default();

                        code.push_str(&format!(
                            "        // Condition: {} {} {}\n",
                            field, op, value_str
                        ));
                        code.push_str("        if matched_branch.is_none() {\n");
                        code.push_str(&format!(
                            "            let field_val = ctx.state.get(\"{}\").and_then(|v| v.as_str()).unwrap_or(\"\");\n",
                            field
                        ));

                        generate_condition_check(&mut code, op, &value_str, port, field);
                        code.push_str("        }\n");
                    }

                    // Default branch
                    if let Some(default) = &config.default_branch {
                        code.push_str(&format!(
                            "        let branch = matched_branch.unwrap_or(\"{}\");\n",
                            default
                        ));
                    } else {
                        code.push_str(
                            "        let branch = matched_branch.unwrap_or(\"default\");\n",
                        );
                    }

                    code.push_str(&format!(
                        "        Ok(NodeOutput::new().with_update(\"{}\", json!(branch)))\n",
                        output_key
                    ));
                }
                crate::codegen::action_nodes::EvaluationMode::AllMatch => {
                    // All match (fan-out): evaluate all conditions, store matched branches in state
                    // All connected branches execute via direct edges regardless — this is for observability
                    code.push_str("        let mut matched_branches: Vec<String> = Vec::new();\n\n");

                    for condition in &config.conditions {
                        let field = &condition.field;
                        let op = &condition.operator;
                        let port = &condition.output_port;
                        let value_str = condition
                            .value
                            .as_ref()
                            .map(|v| match v {
                                serde_json::Value::String(s) => s.clone(),
                                other => other.to_string(),
                            })
                            .unwrap_or_default();

                        code.push_str(&format!(
                            "        // Condition: {} {} {}\n",
                            field, op, value_str
                        ));
                        code.push_str("        {\n");
                        code.push_str(&format!(
                            "            let field_val = ctx.state.get(\"{}\").and_then(|v| v.as_str()).unwrap_or(\"\");\n",
                            field
                        ));

                        generate_all_match_condition_check(&mut code, op, &value_str, port, field);
                        code.push_str("        }\n");
                    }

                    // Store matched branches as a JSON array for observability/debugging
                    code.push_str(&format!(
                        "\n        Ok(NodeOutput::new().with_update(\"{}\", json!(matched_branches)))\n",
                        output_key
                    ));
                }
            }

            code.push_str("    });\n\n");
        }
        ActionNodeConfig::Loop(config) => {
            code.push_str(&format!("    // Action Node: {} (Loop)\n", config.standard.name));
            code.push_str(&format!("    let {}_node = adk_graph::node::FunctionNode::new(\"{}\", |ctx| async move {{\n", node_id, node_id));
            code.push_str("        let mut output = NodeOutput::new();\n");

            match config.loop_type {
                crate::codegen::action_nodes::LoopType::ForEach => {
                    let source = config.for_each.as_ref().map(|f| f.source_array.as_str()).unwrap_or("items");
                    let item_var = config.for_each.as_ref().map(|f| f.item_var.as_str()).unwrap_or("item");
                    let index_var = config.for_each.as_ref().map(|f| f.index_var.as_str()).unwrap_or("index");
                    let counter_key = format!("{}_loop_index", node_id);
                    let done_key = format!("{}_loop_done", node_id);

                    code.push_str(&format!("        // forEach: iterate over '{}'\n", source));
                    code.push_str(&format!("        let source_arr = ctx.state.get(\"{}\").and_then(|v| v.as_array()).cloned().unwrap_or_default();\n", source));
                    code.push_str(&format!("        let idx = ctx.state.get(\"{}\").and_then(|v| v.as_u64()).unwrap_or(0) as usize;\n", counter_key));
                    code.push_str("        if idx < source_arr.len() {\n");
                    code.push_str(&format!("            let current_item = source_arr[idx].clone();\n"));
                    code.push_str(&format!("            output = output.with_update(\"{}\", current_item);\n", item_var));
                    code.push_str(&format!("            output = output.with_update(\"{}\", json!(idx));\n", index_var));
                    code.push_str(&format!("            output = output.with_update(\"{}\", json!(idx + 1));\n", counter_key));
                    code.push_str(&format!("            output = output.with_update(\"{}\", json!(false));\n", done_key));

                    // Collect results
                    if config.results.collect {
                        let agg_key = config.results.aggregation_key.as_deref().unwrap_or("loop_results");
                        code.push_str(&format!("            // Collect previous iteration result if any\n"));
                        code.push_str(&format!("            let mut results = ctx.state.get(\"{}\").and_then(|v| v.as_array()).cloned().unwrap_or_default();\n", agg_key));
                        code.push_str("            if idx > 0 {\n");
                        code.push_str("                // Capture the response from the previous iteration\n");
                        code.push_str("                if let Some(resp) = ctx.state.get(\"response\") {\n");
                        code.push_str("                    results.push(resp.clone());\n");
                        code.push_str("                }\n");
                        code.push_str("            }\n");
                        code.push_str(&format!("            output = output.with_update(\"{}\", json!(results));\n", agg_key));
                    }

                    code.push_str("        } else {\n");
                    code.push_str(&format!("            output = output.with_update(\"{}\", json!(true));\n", done_key));

                    // Final collection on completion
                    if config.results.collect {
                        let agg_key = config.results.aggregation_key.as_deref().unwrap_or("loop_results");
                        code.push_str(&format!("            let mut results = ctx.state.get(\"{}\").and_then(|v| v.as_array()).cloned().unwrap_or_default();\n", agg_key));
                        code.push_str("            if let Some(resp) = ctx.state.get(\"response\") {\n");
                        code.push_str("                results.push(resp.clone());\n");
                        code.push_str("            }\n");
                        code.push_str(&format!("            output = output.with_update(\"{}\", json!(results));\n", agg_key));
                    }

                    code.push_str("        }\n");
                }
                crate::codegen::action_nodes::LoopType::Times => {
                    let count = config.times.as_ref()
                        .map(|t| match &t.count {
                            serde_json::Value::Number(n) => n.as_u64().unwrap_or(3) as usize,
                            _ => 3,
                        })
                        .unwrap_or(3);
                    let counter_key = format!("{}_loop_index", node_id);
                    let done_key = format!("{}_loop_done", node_id);

                    code.push_str(&format!("        // times: repeat {} times\n", count));
                    code.push_str(&format!("        let idx = ctx.state.get(\"{}\").and_then(|v| v.as_u64()).unwrap_or(0) as usize;\n", counter_key));
                    code.push_str(&format!("        if idx < {} {{\n", count));
                    code.push_str(&format!("            output = output.with_update(\"index\", json!(idx));\n"));
                    code.push_str(&format!("            output = output.with_update(\"{}\", json!(idx + 1));\n", counter_key));
                    code.push_str(&format!("            output = output.with_update(\"{}\", json!(false));\n", done_key));

                    if config.results.collect {
                        let agg_key = config.results.aggregation_key.as_deref().unwrap_or("loop_results");
                        code.push_str(&format!("            let mut results = ctx.state.get(\"{}\").and_then(|v| v.as_array()).cloned().unwrap_or_default();\n", agg_key));
                        code.push_str("            if idx > 0 {\n");
                        code.push_str("                if let Some(resp) = ctx.state.get(\"response\") {\n");
                        code.push_str("                    results.push(resp.clone());\n");
                        code.push_str("                }\n");
                        code.push_str("            }\n");
                        code.push_str(&format!("            output = output.with_update(\"{}\", json!(results));\n", agg_key));
                    }

                    code.push_str("        } else {\n");
                    code.push_str(&format!("            output = output.with_update(\"{}\", json!(true));\n", done_key));

                    if config.results.collect {
                        let agg_key = config.results.aggregation_key.as_deref().unwrap_or("loop_results");
                        code.push_str(&format!("            let mut results = ctx.state.get(\"{}\").and_then(|v| v.as_array()).cloned().unwrap_or_default();\n", agg_key));
                        code.push_str("            if let Some(resp) = ctx.state.get(\"response\") {\n");
                        code.push_str("                results.push(resp.clone());\n");
                        code.push_str("            }\n");
                        code.push_str(&format!("            output = output.with_update(\"{}\", json!(results));\n", agg_key));
                    }

                    code.push_str("        }\n");
                }
                crate::codegen::action_nodes::LoopType::While => {
                    let condition_field = config.while_config.as_ref()
                        .map(|w| w.condition.as_str())
                        .unwrap_or("should_continue");
                    let counter_key = format!("{}_loop_index", node_id);
                    let done_key = format!("{}_loop_done", node_id);

                    code.push_str(&format!("        // while: loop while '{}' is truthy\n", condition_field));
                    code.push_str(&format!("        let idx = ctx.state.get(\"{}\").and_then(|v| v.as_u64()).unwrap_or(0) as usize;\n", counter_key));
                    code.push_str(&format!("        let cond_val = ctx.state.get(\"{}\");\n", condition_field));
                    code.push_str("        let should_continue = match cond_val {\n");
                    code.push_str("            Some(v) if v.is_boolean() => v.as_bool().unwrap_or(false),\n");
                    code.push_str("            Some(v) if v.is_string() => !v.as_str().unwrap_or(\"\").is_empty() && v.as_str() != Some(\"false\"),\n");
                    code.push_str("            Some(v) if v.is_number() => v.as_f64().unwrap_or(0.0) != 0.0,\n");
                    code.push_str("            Some(v) if v.is_null() => false,\n");
                    code.push_str("            Some(_) => true,\n");
                    code.push_str("            None => false,\n");
                    code.push_str("        };\n");
                    code.push_str("        if should_continue {\n");
                    code.push_str(&format!("            output = output.with_update(\"index\", json!(idx));\n"));
                    code.push_str(&format!("            output = output.with_update(\"{}\", json!(idx + 1));\n", counter_key));
                    code.push_str(&format!("            output = output.with_update(\"{}\", json!(false));\n", done_key));
                    code.push_str("        } else {\n");
                    code.push_str(&format!("            output = output.with_update(\"{}\", json!(true));\n", done_key));
                    code.push_str("        }\n");
                }
            }

            code.push_str("        Ok(output)\n");
            code.push_str("    });\n\n");
        }
        ActionNodeConfig::Merge(config) => {
            code.push_str(&format!("    // Action Node: {} (Merge)\n", config.standard.name));
            code.push_str(&format!("    let {}_node = adk_graph::node::FunctionNode::new(\"{}\", |ctx| async move {{\n", node_id, node_id));
            code.push_str("        let mut output = NodeOutput::new();\n");

            let output_key = if config.standard.mapping.output_key.is_empty() {
                "merged"
            } else {
                &config.standard.mapping.output_key
            };

            // Determine branch keys for collecting results
            let branch_keys = config.branch_keys.as_deref().unwrap_or(&[]);

            match config.combine_strategy {
                crate::codegen::action_nodes::CombineStrategy::Array => {
                    code.push_str("        // Combine strategy: array — collect branch outputs into an array\n");
                    code.push_str("        let mut results: Vec<serde_json::Value> = Vec::new();\n");
                    if branch_keys.is_empty() {
                        // Collect all non-system state values
                        code.push_str("        for (k, v) in ctx.state.iter() {\n");
                        code.push_str("            if k != \"message\" && k != \"classification\" {\n");
                        code.push_str("                results.push(v.clone());\n");
                        code.push_str("            }\n");
                        code.push_str("        }\n");
                    } else {
                        for key in branch_keys {
                            code.push_str(&format!("        if let Some(v) = ctx.state.get(\"{}\") {{\n", key));
                            code.push_str("            results.push(v.clone());\n");
                            code.push_str("        }\n");
                        }
                    }
                    code.push_str(&format!("        output = output.with_update(\"{}\", json!(results));\n", output_key));
                }
                crate::codegen::action_nodes::CombineStrategy::Object => {
                    code.push_str("        // Combine strategy: object — merge branch outputs into an object\n");
                    code.push_str("        let mut merged = serde_json::Map::new();\n");
                    if branch_keys.is_empty() {
                        code.push_str("        for (k, v) in ctx.state.iter() {\n");
                        code.push_str("            if k != \"message\" && k != \"classification\" {\n");
                        code.push_str("                merged.insert(k.clone(), v.clone());\n");
                        code.push_str("            }\n");
                        code.push_str("        }\n");
                    } else {
                        for key in branch_keys {
                            code.push_str(&format!("        if let Some(v) = ctx.state.get(\"{}\") {{\n", key));
                            code.push_str(&format!("            merged.insert(\"{}\".to_string(), v.clone());\n", key));
                            code.push_str("        }\n");
                        }
                    }
                    code.push_str(&format!("        output = output.with_update(\"{}\", json!(merged));\n", output_key));
                }
                crate::codegen::action_nodes::CombineStrategy::First => {
                    code.push_str("        // Combine strategy: first — use first available branch output\n");
                    if branch_keys.is_empty() {
                        code.push_str("        let first_val = ctx.state.get(\"response\").cloned().unwrap_or(json!(null));\n");
                    } else {
                        code.push_str("        let first_val = None\n");
                        for key in branch_keys {
                            code.push_str(&format!("            .or_else(|| ctx.state.get(\"{}\").cloned())\n", key));
                        }
                        code.push_str("            .unwrap_or(json!(null));\n");
                    }
                    code.push_str(&format!("        output = output.with_update(\"{}\", first_val);\n", output_key));
                }
                crate::codegen::action_nodes::CombineStrategy::Last => {
                    code.push_str("        // Combine strategy: last — use last available branch output\n");
                    if branch_keys.is_empty() {
                        code.push_str("        let last_val = ctx.state.get(\"response\").cloned().unwrap_or(json!(null));\n");
                    } else {
                        code.push_str("        let last_val = None\n");
                        for key in branch_keys.iter().rev() {
                            code.push_str(&format!("            .or_else(|| ctx.state.get(\"{}\").cloned())\n", key));
                        }
                        code.push_str("            .unwrap_or(json!(null));\n");
                    }
                    code.push_str(&format!("        output = output.with_update(\"{}\", last_val);\n", output_key));
                }
            }

            code.push_str("        Ok(output)\n");
            code.push_str("    });\n\n");
        }
        // Other action node types can be added here
        _ => {
            // For unsupported action nodes, generate a pass-through node
            let standard = node.standard();
            code.push_str(&format!(
                "    // Action Node: {} ({})\n",
                standard.name,
                node.node_type()
            ));
            code.push_str(&format!("    let {}_node = adk_graph::node::FunctionNode::new(\"{}\", |_ctx| async move {{\n", node_id, node_id));
            code.push_str("        Ok(NodeOutput::new())\n");
            code.push_str("    });\n\n");
        }
    }

    code
}

fn generate_cargo_toml(project: &ProjectSchema) -> String {
    let mut name = project
        .name
        .to_lowercase()
        .replace(' ', "_")
        .replace(|c: char| !c.is_alphanumeric() && c != '_', "");
    // Cargo package names can't start with a digit
    if name.is_empty() || name.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
        name = format!("project_{}", name);
    }

    // Get ADK version and Rust edition from project settings (with defaults)
    let adk_version = project.settings.adk_version.as_deref().unwrap_or("0.2.1");
    let rust_edition = project.settings.rust_edition.as_deref().unwrap_or("2024");

    // Check if any function tool code uses specific crates
    let code_uses = |pattern: &str| -> bool {
        project.tool_configs.values().any(|tc| {
            if let ToolConfig::Function(fc) = tc { fc.code.contains(pattern) } else { false }
        })
    };

    let needs_reqwest = code_uses("reqwest::");
    let needs_lettre = code_uses("lettre::");
    let needs_base64 = code_uses("base64::");

    // Use path dependencies in dev mode, version dependencies in prod
    let use_path_deps = std::env::var("ADK_DEV_MODE").is_ok();

    let adk_root = "/data/projects/production/adk/adk-rust";

    let adk_deps = if use_path_deps {
        format!(
            r#"adk-agent = {{ path = "{}/adk-agent" }}
adk-core = {{ path = "{}/adk-core" }}
adk-model = {{ path = "{}/adk-model" }}
adk-tool = {{ path = "{}/adk-tool" }}
adk-graph = {{ path = "{}/adk-graph" }}"#,
            adk_root, adk_root, adk_root, adk_root, adk_root
        )
    } else {
        format!(
            r#"adk-agent = "{}"
adk-core = "{}"
adk-model = "{}"
adk-tool = "{}"
adk-graph = "{}""#,
            adk_version, adk_version, adk_version, adk_version, adk_version
        )
    };

    // No patch section needed - adk-gemini is a workspace member
    let patch_section = String::new();

    let mut deps = format!(
        r#"[package]
name = "{}"
version = "0.1.0"
edition = "{}"

[dependencies]
{}
tokio = {{ version = "1", features = ["full", "macros"] }}
tokio-stream = "0.1"
anyhow = "1"
serde = {{ version = "1", features = ["derive"] }}
serde_json = "1"
schemars = "0.8"
tracing-subscriber = {{ version = "0.3", features = ["json", "env-filter"] }}
uuid = {{ version = "1", features = ["v4"] }}
"#,
        name, rust_edition, adk_deps
    );

    if needs_reqwest {
        deps.push_str("reqwest = { version = \"0.12\", features = [\"json\"] }\n");
    }
    if needs_lettre {
        deps.push_str("lettre = \"0.11\"\n");
    }
    if needs_base64 {
        deps.push_str("base64 = \"0.21\"\n");
    }

    // Add rmcp if any agent uses MCP (handles mcp, mcp_1, mcp_2, etc.)
    let uses_mcp = project
        .agents
        .values()
        .any(|a| a.tools.iter().any(|t| t == "mcp" || t.starts_with("mcp_")));
    if uses_mcp {
        deps.push_str(
            "rmcp = { version = \"0.9\", features = [\"client\", \"transport-child-process\"] }\n",
        );
        deps.push_str("async-trait = \"0.1\"\n");
    }

    // Add adk-browser if any agent uses browser tool
    let uses_browser = project.agents.values().any(|a| a.tools.contains(&"browser".to_string()));
    if uses_browser {
        if use_path_deps {
            deps.push_str(&format!("adk-browser = {{ path = \"{}/adk-browser\" }}\n", adk_root));
        } else {
            deps.push_str(&format!("adk-browser = \"{}\"\n", adk_version));
        }
        // async-trait needed for MinimalContext if not already added by MCP
        if !uses_mcp {
            deps.push_str("async-trait = \"0.1\"\n");
        }
    }

    // Add patch section at the end (only in dev mode)
    deps.push_str(&patch_section);

    deps
}
