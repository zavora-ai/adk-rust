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

/// Detect the LLM provider from a model name string.
/// Mirrors the TypeScript `detectProviderFromModel()` in `ui/src/data/models.ts`.
fn detect_provider(model: &str) -> &'static str {
    let m = model.to_lowercase();
    if m.contains("gemini") || m.contains("gemma") {
        "gemini"
    } else if m.contains("gpt")
        || m.contains("o1")
        || m.contains("o3")
        || m.contains("o4")
        || m.contains("codex")
    {
        "openai"
    } else if m.contains("claude")
        && !m.starts_with("us.")
        && !m.starts_with("eu.")
        && !m.starts_with("ap.")
    {
        "anthropic"
    } else if m.starts_with("us.") || m.starts_with("eu.") || m.starts_with("ap.") {
        // Bedrock inference profile IDs (e.g. "us.anthropic.claude-sonnet-4-6")
        "bedrock"
    } else if m.contains("deepseek") && !m.contains(':') {
        // DeepSeek API (no colon = not Ollama tag format)
        "deepseek"
    } else if m.contains("sonar") {
        "perplexity"
    } else if (m.contains("mistral-large")
        || m.contains("mistral-small")
        || m.contains("codestral"))
        && !m.contains(':')
    {
        "mistral"
    } else if m.contains("accounts/fireworks/") {
        "fireworks"
    } else if m.contains("-turbo") && m.contains('/') {
        "together"
    } else if m.contains("cohere-command") || (m.contains("mistral") && m.contains("2024")) {
        "azure-ai"
    } else if m.contains("llama") || m.contains("mixtral") {
        // Ollama-style tags have colons (e.g. "llama3.2:3b")
        if m.contains(':') {
            "ollama"
        } else if model.starts_with("Meta-Llama") {
            "sambanova"
        } else if m.starts_with("llama-")
            && m.chars().nth(6).map(|c| c.is_ascii_digit()).unwrap_or(false)
        {
            "cerebras"
        } else {
            "groq"
        }
    } else if m.contains("qwen")
        || m.contains("mistral")
        || m.contains("codellama")
        || m.contains("devstral")
    {
        "ollama"
    } else {
        "gemini" // default
    }
}

/// Collect the set of unique providers used across all agents in a project.
fn collect_providers(project: &ProjectSchema) -> std::collections::HashSet<&'static str> {
    let mut providers = std::collections::HashSet::new();
    for agent in project.agents.values() {
        let model = agent.model.as_deref().unwrap_or("gemini-2.5-flash");
        providers.insert(detect_provider(model));
    }
    // If project has a default_provider set, include it
    if let Some(ref dp) = project.settings.default_provider {
        let p = match dp.as_str() {
            "gemini" | "openai" | "anthropic" | "deepseek" | "groq" | "ollama" | "fireworks"
            | "together" | "mistral" | "perplexity" | "cerebras" | "sambanova" | "bedrock"
            | "azure-ai" => dp.as_str(),
            _ => "gemini",
        };
        providers.insert(match p {
            "openai" => "openai",
            "anthropic" => "anthropic",
            "deepseek" => "deepseek",
            "groq" => "groq",
            "ollama" => "ollama",
            "fireworks" => "fireworks",
            "together" => "together",
            "mistral" => "mistral",
            "perplexity" => "perplexity",
            "cerebras" => "cerebras",
            "sambanova" => "sambanova",
            "bedrock" => "bedrock",
            "azure-ai" => "azure-ai",
            _ => "gemini",
        });
    }
    providers
}

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
        comment.push(')');

        // Add brief instruction summary if available
        if !agent.instruction.is_empty() {
            let brief = truncate_instruction(&agent.instruction, 60);
            comment.push_str(": ");
            comment.push_str(&brief);
        }
        comment.push('\n');

        // List tools if any
        if !agent.tools.is_empty() {
            comment.push_str("//!   Tools: ");
            comment.push_str(&agent.tools.join(", "));
            comment.push('\n');
        }

        // List sub-agents if any
        if !agent.sub_agents.is_empty() {
            comment.push_str("//!   Sub-agents: ");
            comment.push_str(&agent.sub_agents.join(" → "));
            comment.push('\n');
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
                comment.push(']');
            }
            comment.push('\n');
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
            diagram.push(']');
            break; // Stop at branching point
        }
    }

    diagram.push('\n');
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
    // Import model providers based on what agents actually use
    let providers = collect_providers(project);
    if providers.contains("gemini") {
        code.push_str("use adk_model::gemini::GeminiModel;\n");
    }
    if providers.contains("openai") {
        code.push_str("use adk_model::openai::{OpenAIClient, OpenAIConfig};\n");
    }
    if providers.contains("anthropic") {
        code.push_str("use adk_model::anthropic::{AnthropicClient, AnthropicConfig};\n");
    }
    if providers.contains("deepseek") {
        code.push_str("use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig};\n");
    }
    if providers.contains("groq") {
        code.push_str("use adk_model::groq::{GroqClient, GroqConfig};\n");
    }
    if providers.contains("ollama") {
        code.push_str("use adk_model::ollama::{OllamaModel, OllamaConfig};\n");
    }
    if providers.contains("fireworks") {
        code.push_str("use adk_model::fireworks::{FireworksClient, FireworksConfig};\n");
    }
    if providers.contains("together") {
        code.push_str("use adk_model::together::{TogetherClient, TogetherConfig};\n");
    }
    if providers.contains("mistral") {
        code.push_str("use adk_model::mistral::{MistralClient, MistralConfig};\n");
    }
    if providers.contains("perplexity") {
        code.push_str("use adk_model::perplexity::{PerplexityClient, PerplexityConfig};\n");
    }
    if providers.contains("cerebras") {
        code.push_str("use adk_model::cerebras::{CerebrasClient, CerebrasConfig};\n");
    }
    if providers.contains("sambanova") {
        code.push_str("use adk_model::sambanova::{SambaNovaClient, SambaNovaConfig};\n");
    }
    if providers.contains("bedrock") {
        code.push_str("use adk_model::bedrock::{BedrockClient, BedrockConfig};\n");
    }
    if providers.contains("azure-ai") {
        code.push_str("use adk_model::azure_ai::{AzureAIClient, AzureAIConfig};\n");
    }
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

    // Add js_value_to_json helper if any Code action nodes exist
    {
        use crate::codegen::action_nodes::ActionNodeConfig;
        let has_code_nodes =
            project.action_nodes.values().any(|n| matches!(n, ActionNodeConfig::Code(_)));
        if has_code_nodes {
            code.push_str("/// Convert a boa_engine JsValue to serde_json::Value\n");
            code.push_str("fn js_value_to_json(val: &boa_engine::JsValue, ctx: &mut boa_engine::Context) -> serde_json::Value {\n");
            code.push_str(
                "    // Handle Undefined/Null before to_json (which panics on Undefined)\n",
            );
            code.push_str("    if val.is_undefined() || val.is_null() {\n");
            code.push_str("        return serde_json::Value::Null;\n");
            code.push_str("    }\n");
            code.push_str("    match val.to_json(ctx) {\n");
            code.push_str("        Ok(json) => json,\n");
            code.push_str("        Err(_) => {\n");
            code.push_str("            match val {\n");
            code.push_str("                boa_engine::JsValue::Boolean(b) => json!(*b),\n");
            code.push_str("                boa_engine::JsValue::Integer(n) => json!(*n),\n");
            code.push_str("                boa_engine::JsValue::Rational(n) => if n.is_finite() { json!(*n) } else { serde_json::Value::Null },\n");
            code.push_str("                boa_engine::JsValue::String(s) => json!(s.to_std_string_escaped()),\n");
            code.push_str("                _ => json!(val.display().to_string()),\n");
            code.push_str("            }\n");
            code.push_str("        }\n");
            code.push_str("    }\n");
            code.push_str("}\n\n");
        }
    }

    code.push_str("#[tokio::main]\n");
    code.push_str("async fn main() -> Result<()> {\n");
    // Initialize tracing with JSON output
    code.push_str("    // Initialize tracing\n");
    code.push_str("    fmt().with_env_filter(EnvFilter::from_default_env().add_directive(\"adk=info\".parse()?)).json().with_writer(std::io::stderr).init();\n\n");
    // Resolve API keys for each provider used
    if providers.contains("gemini") {
        code.push_str("    let gemini_api_key = std::env::var(\"GOOGLE_API_KEY\")\n");
        code.push_str("        .or_else(|_| std::env::var(\"GEMINI_API_KEY\"))\n");
        code.push_str("        .expect(\"GOOGLE_API_KEY or GEMINI_API_KEY must be set\");\n\n");
    }
    if providers.contains("openai") {
        code.push_str("    let openai_api_key = std::env::var(\"OPENAI_API_KEY\")\n");
        code.push_str("        .expect(\"OPENAI_API_KEY must be set\");\n\n");
    }
    if providers.contains("anthropic") {
        code.push_str("    let anthropic_api_key = std::env::var(\"ANTHROPIC_API_KEY\")\n");
        code.push_str("        .expect(\"ANTHROPIC_API_KEY must be set\");\n\n");
    }
    if providers.contains("deepseek") {
        code.push_str("    let deepseek_api_key = std::env::var(\"DEEPSEEK_API_KEY\")\n");
        code.push_str("        .expect(\"DEEPSEEK_API_KEY must be set\");\n\n");
    }
    if providers.contains("groq") {
        code.push_str("    let groq_api_key = std::env::var(\"GROQ_API_KEY\")\n");
        code.push_str("        .expect(\"GROQ_API_KEY must be set\");\n\n");
    }
    if providers.contains("ollama") {
        code.push_str("    let _ollama_host = std::env::var(\"OLLAMA_HOST\")\n");
        code.push_str("        .unwrap_or_else(|_| \"http://localhost:11434\".to_string());\n\n");
    }
    if providers.contains("fireworks") {
        code.push_str("    let fireworks_api_key = std::env::var(\"FIREWORKS_API_KEY\")\n");
        code.push_str("        .expect(\"FIREWORKS_API_KEY must be set\");\n\n");
    }
    if providers.contains("together") {
        code.push_str("    let together_api_key = std::env::var(\"TOGETHER_API_KEY\")\n");
        code.push_str("        .expect(\"TOGETHER_API_KEY must be set\");\n\n");
    }
    if providers.contains("mistral") {
        code.push_str("    let mistral_api_key = std::env::var(\"MISTRAL_API_KEY\")\n");
        code.push_str("        .expect(\"MISTRAL_API_KEY must be set\");\n\n");
    }
    if providers.contains("perplexity") {
        code.push_str("    let perplexity_api_key = std::env::var(\"PERPLEXITY_API_KEY\")\n");
        code.push_str("        .expect(\"PERPLEXITY_API_KEY must be set\");\n\n");
    }
    if providers.contains("cerebras") {
        code.push_str("    let cerebras_api_key = std::env::var(\"CEREBRAS_API_KEY\")\n");
        code.push_str("        .expect(\"CEREBRAS_API_KEY must be set\");\n\n");
    }
    if providers.contains("sambanova") {
        code.push_str("    let sambanova_api_key = std::env::var(\"SAMBANOVA_API_KEY\")\n");
        code.push_str("        .expect(\"SAMBANOVA_API_KEY must be set\");\n\n");
    }
    if providers.contains("bedrock") {
        code.push_str("    let bedrock_region = std::env::var(\"AWS_DEFAULT_REGION\")\n");
        code.push_str("        .unwrap_or_else(|_| \"us-east-1\".to_string());\n\n");
    }
    if providers.contains("azure-ai") {
        code.push_str("    let azure_ai_endpoint = std::env::var(\"AZURE_AI_ENDPOINT\")\n");
        code.push_str("        .expect(\"AZURE_AI_ENDPOINT must be set\");\n");
        code.push_str("    let azure_ai_api_key = std::env::var(\"AZURE_AI_API_KEY\")\n");
        code.push_str("        .expect(\"AZURE_AI_API_KEY must be set\");\n\n");
    }

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
            && project
                .action_nodes
                .get(&edge.from)
                .map(|n| !matches!(n, ActionNodeConfig::Loop(_)))
                .unwrap_or(true)
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

    // Identify all_match switch nodes early — needed to detect parallel branch agents
    let all_match_switch_nodes_early: std::collections::HashSet<&str> = project
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

    // Identify agents that are in parallel fan-out branches (predecessor is an all_match switch)
    // These agents must write to unique output keys to avoid overwriting each other
    let mut parallel_branch_agents: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for edge in &project.workflow.edges {
        if all_match_switch_nodes_early.contains(edge.from.as_str()) {
            // This edge goes from an all_match switch to a branch target
            // If the target is an agent, it's in a parallel branch
            if project.agents.contains_key(&edge.to) {
                parallel_branch_agents.insert(edge.to.clone());
            }
        }
    }

    // Generate all agent nodes with their predecessors
    for agent_id in &top_level {
        if let Some(agent) = project.agents.get(*agent_id) {
            // Get first predecessor for backward compat (most nodes have exactly one)
            let predecessors = predecessor_map.get(agent_id.as_str());
            let first_predecessor = predecessors.and_then(|v| v.first().copied());
            let is_parallel = parallel_branch_agents.contains(*agent_id);
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
                        is_parallel,
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
        .filter(|(_, node)| !matches!(node, ActionNodeConfig::Trigger(_)))
        .collect();

    for (node_id, node) in &executable_action_nodes {
        code.push_str(&generate_action_node_function(
            node_id,
            node,
            &predecessor_map,
            &parallel_branch_agents,
        ));
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
                    let agg_key =
                        config.results.aggregation_key.as_deref().unwrap_or("loop_results");
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
    let base_channels =
        vec!["message".to_string(), "classification".to_string(), "response".to_string()];
    // Add unique output channels for parallel-branch agents
    let parallel_channels: Vec<String> =
        parallel_branch_agents.iter().map(|id| format!("{}_response", id)).collect();
    let all_channels: Vec<String> =
        base_channels.into_iter().chain(extra_channels).chain(parallel_channels).collect();
    let channel_list =
        all_channels.iter().map(|c| format!("\"{}\"", c)).collect::<Vec<_>>().join(", ");
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

    let first_match_switch_nodes: std::collections::HashSet<&str> =
        switch_nodes.iter().filter(|id| !all_match_switch_nodes.contains(*id)).copied().collect();

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
            let target = if edge.to == "END" { "END".to_string() } else { edge.to.clone() };
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
                    let target_str =
                        if target == "END" { "END".to_string() } else { format!("\"{}\"", target) };
                    format!("(\"{}\", {})", port, target_str)
                })
                .collect();

            code.push_str(&format!(
                "        // Switch node: {} - conditional routing by '{}'\n",
                switch_id, output_key
            ));
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
        let exit_targets =
            loop_exit_map.get(loop_id).cloned().unwrap_or_else(|| vec!["END".to_string()]);

        if let Some(body_first) = body_targets.first() {
            let exit_first = exit_targets.first().map(|s| s.as_str()).unwrap_or("END");

            let body_target_str = format!("\"{}\"", body_first);
            let exit_target_str =
                if exit_first == "END" { "END".to_string() } else { format!("\"{}\"", exit_first) };

            code.push_str(&format!(
                "        // Loop node: {} - conditional cycle by '{}'\n",
                loop_id, done_key
            ));
            code.push_str("        .add_conditional_edges(\n");
            code.push_str(&format!("            \"{}\",\n", loop_id));
            code.push_str(&format!(
                "            Router::by_bool(\"{}\", \"exit\", \"body\"),\n",
                done_key
            ));
            code.push_str(&format!(
                "            [(\"body\", {}), (\"exit\", {})],\n",
                body_target_str, exit_target_str
            ));
            code.push_str("        )\n");

            // Add back-edge from last body node to loop node (cycle)
            for (back_from, back_to) in &loop_back_edges {
                if back_to.as_str() == *loop_id {
                    code.push_str(&format!(
                        "        .add_edge(\"{}\", \"{}\")\n",
                        back_from, loop_id
                    ));
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
    code.push_str("                    // Emit THINKING: prefix for Part::Thinking content (Requirements 6.1, 6.2)\n");
    code.push_str("                    if let adk_graph::StreamEvent::Custom { event_type, data, .. } = &e {\n");
    code.push_str("                        if event_type == \"agent_event\" {\n");
    code.push_str("                            if let Some(parts) = data.pointer(\"/content/parts\").and_then(|v| v.as_array()) {\n");
    code.push_str("                                for part in parts {\n");
    code.push_str("                                    if let Some(thinking) = part.get(\"thinking\").and_then(|v| v.as_str()) {\n");
    code.push_str("                                        if !thinking.is_empty() {\n");
    code.push_str("                                            println!(\"THINKING:{}\", serde_json::to_string(&thinking).unwrap_or_default());\n");
    code.push_str("                                        }\n");
    code.push_str("                                    }\n");
    code.push_str("                                }\n");
    code.push_str("                            }\n");
    code.push_str("                        }\n");
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
    let model = agent.model.as_deref().unwrap_or("gemini-2.5-flash");

    code.push_str(&format!("    // Router: {}\n", id));
    code.push_str(&format!("    let {}_llm = Arc::new(\n", id));
    code.push_str(&format!("        LlmAgentBuilder::new(\"{}\")\n", id));

    // Generate provider-specific model construction
    let provider = detect_provider(model);
    match provider {
        "openai" => {
            code.push_str(&format!(
                "            .model(Arc::new(OpenAIClient::new(OpenAIConfig::new(&openai_api_key, \"{}\"))?))\n",
                model
            ));
        }
        "anthropic" => {
            code.push_str(&format!(
                "            .model(Arc::new(AnthropicClient::new(AnthropicConfig::new(&anthropic_api_key, \"{}\"))?))\n",
                model
            ));
        }
        "deepseek" => {
            let m = model.to_lowercase();
            if m.contains("reasoner") || m.contains("r1") {
                code.push_str(
                    "            .model(Arc::new(DeepSeekClient::reasoner(&deepseek_api_key)?))\n",
                );
            } else {
                code.push_str(
                    "            .model(Arc::new(DeepSeekClient::chat(&deepseek_api_key)?))\n",
                );
            }
        }
        "groq" => {
            code.push_str(&format!(
                "            .model(Arc::new(GroqClient::new(GroqConfig::new(&groq_api_key, \"{}\"))?))\n",
                model
            ));
        }
        "ollama" => {
            code.push_str(&format!(
                "            .model(Arc::new(OllamaModel::new(OllamaConfig::new(\"{}\"))?))\n",
                model
            ));
        }
        _ => {
            // Default: Gemini
            code.push_str(&format!(
                "            .model(Arc::new(GeminiModel::new(&gemini_api_key, \"{}\")?))\n",
                model
            ));
        }
    }

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
    is_parallel_branch: bool,
) -> String {
    let mut code = String::new();
    let model = agent.model.as_deref().unwrap_or("gemini-2.5-flash");

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

    // Generate provider-specific model construction
    let provider = detect_provider(model);
    match provider {
        "openai" => {
            code.push_str(&format!(
                "        .model(Arc::new(OpenAIClient::new(OpenAIConfig::new(&openai_api_key, \"{}\"))?));\n",
                model
            ));
        }
        "anthropic" => {
            code.push_str(&format!(
                "        .model(Arc::new(AnthropicClient::new(AnthropicConfig::new(&anthropic_api_key, \"{}\"))?));\n",
                model
            ));
        }
        "deepseek" => {
            // Use DeepSeekClient convenience constructors for known models
            let m = model.to_lowercase();
            if m.contains("reasoner") || m.contains("r1") {
                code.push_str(
                    "        .model(Arc::new(DeepSeekClient::reasoner(&deepseek_api_key)?));\n",
                );
            } else {
                code.push_str(
                    "        .model(Arc::new(DeepSeekClient::chat(&deepseek_api_key)?));\n",
                );
            }
        }
        "groq" => {
            code.push_str(&format!(
                "        .model(Arc::new(GroqClient::new(GroqConfig::new(&groq_api_key, \"{}\"))?));\n",
                model
            ));
        }
        "ollama" => {
            code.push_str(&format!(
                "        .model(Arc::new(OllamaModel::new(OllamaConfig::new(\"{}\"))?));\n",
                model
            ));
        }
        "fireworks" => {
            code.push_str(&format!(
                "        .model(Arc::new(FireworksClient::new(FireworksConfig::new(&fireworks_api_key, \"{}\"))?));\n",
                model
            ));
        }
        "together" => {
            code.push_str(&format!(
                "        .model(Arc::new(TogetherClient::new(TogetherConfig::new(&together_api_key, \"{}\"))?));\n",
                model
            ));
        }
        "mistral" => {
            code.push_str(&format!(
                "        .model(Arc::new(MistralClient::new(MistralConfig::new(&mistral_api_key, \"{}\"))?));\n",
                model
            ));
        }
        "perplexity" => {
            code.push_str(&format!(
                "        .model(Arc::new(PerplexityClient::new(PerplexityConfig::new(&perplexity_api_key, \"{}\"))?));\n",
                model
            ));
        }
        "cerebras" => {
            code.push_str(&format!(
                "        .model(Arc::new(CerebrasClient::new(CerebrasConfig::new(&cerebras_api_key, \"{}\"))?));\n",
                model
            ));
        }
        "sambanova" => {
            code.push_str(&format!(
                "        .model(Arc::new(SambaNovaClient::new(SambaNovaConfig::new(&sambanova_api_key, \"{}\"))?));\n",
                model
            ));
        }
        "bedrock" => {
            code.push_str(&format!(
                "        .model(Arc::new(BedrockClient::new(BedrockConfig::new(&bedrock_region, \"{}\")).await?));\n",
                model
            ));
        }
        "azure-ai" => {
            code.push_str(&format!(
                "        .model(Arc::new(AzureAIClient::new(AzureAIConfig::new(&azure_ai_endpoint, &azure_ai_api_key, \"{}\"))?));\n",
                model
            ));
        }
        _ => {
            // Default: Gemini
            code.push_str(&format!(
                "        .model(Arc::new(GeminiModel::new(&gemini_api_key, \"{}\")?));\n",
                model
            ));
        }
    }

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

    // Add generation config if set
    if let Some(temp) = agent.temperature {
        code.push_str(&format!("    {}_builder = {}_builder.temperature({:.1});\n", id, id, temp));
    }
    if let Some(top_p) = agent.top_p {
        code.push_str(&format!("    {}_builder = {}_builder.top_p({:.2});\n", id, id, top_p));
    }
    if let Some(top_k) = agent.top_k {
        code.push_str(&format!("    {}_builder = {}_builder.top_k({});\n", id, id, top_k));
    }
    if let Some(max_tokens) = agent.max_output_tokens {
        code.push_str(&format!(
            "    {}_builder = {}_builder.max_output_tokens({});\n",
            id, id, max_tokens
        ));
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
        let mut queue: Vec<&str> = if let Some(pred) = predecessor { vec![pred] } else { vec![] };
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
                    _ => {
                        // For any other action node (Http, Wait, etc.),
                        // inject their output_key so the agent sees the result
                        let out_key = &action_node.standard().mapping.output_key;
                        if !out_key.is_empty() && !inject_vars.contains(out_key) {
                            inject_vars.push(out_key.clone());
                        }
                    }
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
    if is_parallel_branch {
        // Parallel branch agents write to a unique key to avoid overwriting each other
        code.push_str(&format!(
            "                updates.insert(\"{}_response\".to_string(), json!(full_text));\n",
            id
        ));
    } else {
        code.push_str(
            "                updates.insert(\"response\".to_string(), json!(full_text));\n",
        );
    }
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
            let model = sub.model.as_deref().unwrap_or("gemini-2.5-flash");
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

            // Generate provider-specific model construction
            let provider = detect_provider(model);
            match provider {
                "openai" => {
                    code.push_str(&format!(
                        "        .model(Arc::new(OpenAIClient::new(OpenAIConfig::new(&openai_api_key, \"{}\"))?));\n",
                        model
                    ));
                }
                "anthropic" => {
                    code.push_str(&format!(
                        "        .model(Arc::new(AnthropicClient::new(AnthropicConfig::new(&anthropic_api_key, \"{}\"))?));\n",
                        model
                    ));
                }
                "deepseek" => {
                    let m = model.to_lowercase();
                    if m.contains("reasoner") || m.contains("r1") {
                        code.push_str(
                            "        .model(Arc::new(DeepSeekClient::reasoner(&deepseek_api_key)?));\n",
                        );
                    } else {
                        code.push_str(
                            "        .model(Arc::new(DeepSeekClient::chat(&deepseek_api_key)?));\n",
                        );
                    }
                }
                "groq" => {
                    code.push_str(&format!(
                        "        .model(Arc::new(GroqClient::new(GroqConfig::new(&groq_api_key, \"{}\"))?));\n",
                        model
                    ));
                }
                "ollama" => {
                    code.push_str(&format!(
                        "        .model(Arc::new(OllamaModel::new(OllamaConfig::new(\"{}\"))?));\n",
                        model
                    ));
                }
                _ => {
                    // Default: Gemini
                    code.push_str(&format!(
                        "        .model(Arc::new(GeminiModel::new(&gemini_api_key, \"{}\")?))",
                        model
                    ));
                    code.push_str(";\n");
                }
            }

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

            // Add generation config if set
            if let Some(temp) = sub.temperature {
                code.push_str(&format!(
                    "    {}_builder = {}_builder.temperature({:.1});\n",
                    sub_id, sub_id, temp
                ));
            }
            if let Some(top_p) = sub.top_p {
                code.push_str(&format!(
                    "    {}_builder = {}_builder.top_p({:.2});\n",
                    sub_id, sub_id, top_p
                ));
            }
            if let Some(top_k) = sub.top_k {
                code.push_str(&format!(
                    "    {}_builder = {}_builder.top_k({});\n",
                    sub_id, sub_id, top_k
                ));
            }
            if let Some(max_tokens) = sub.max_output_tokens {
                code.push_str(&format!(
                    "    {}_builder = {}_builder.max_output_tokens({});\n",
                    sub_id, sub_id, max_tokens
                ));
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
///
/// Helper: generate JSONPath-like extraction code for HTTP response.
/// Supports simple dot-notation paths like "data.items" or "result.name".
fn generate_json_path_extraction(code: &mut String, json_path: &str, output_key: &str) {
    // Strip leading "$." if present (JSONPath convention)
    let path = json_path.strip_prefix("$.").unwrap_or(json_path);
    let segments: Vec<&str> = path.split('.').collect();

    if segments.is_empty() || (segments.len() == 1 && segments[0].is_empty()) {
        // No path — return the whole body
        code.push_str(&format!(
            "                Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"status\": status, \"body\": body }})))\n",
            output_key
        ));
        return;
    }

    code.push_str("                let mut extracted = body.clone();\n");
    for seg in &segments {
        code.push_str(&format!(
            "                extracted = extracted.get(\"{}\").cloned().unwrap_or(json!(null));\n",
            seg.replace('"', "\\\"")
        ));
    }
    code.push_str(&format!(
        "                Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"status\": status, \"body\": extracted }})))\n",
        output_key
    ));
}

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
fn generate_all_match_condition_check(
    code: &mut String,
    op: &str,
    value_str: &str,
    port: &str,
    field: &str,
) {
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
    predecessor_map: &std::collections::HashMap<&str, Vec<&str>>,
    parallel_branch_agents: &std::collections::HashSet<String>,
) -> String {
    use crate::codegen::action_nodes::ActionNodeConfig;

    let mut code = String::new();

    // Check if this node's predecessor is a parallel-branch agent
    // If so, expressions like {{response}} should be rewritten to {{predecessor_response}}
    let parallel_predecessor: Option<&str> = predecessor_map
        .get(node_id)
        .and_then(|preds| preds.iter().find(|p| parallel_branch_agents.contains(**p)))
        .copied();

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
                        let raw_expr = var.value.as_str().unwrap_or("");
                        // If predecessor is a parallel-branch agent, rewrite {{response}} to {{agent_response}}
                        let expr = if let Some(pred_id) = parallel_predecessor {
                            raw_expr
                                .replace("{{response}}", &format!("{{{{{}_response}}}}", pred_id))
                        } else {
                            raw_expr.to_string()
                        };
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
                        let raw_val = var.value.as_str().unwrap_or("");
                        if raw_val.contains("{{") {
                            // String contains template variables — treat like expression
                            let val = if let Some(pred_id) = parallel_predecessor {
                                raw_val.replace(
                                    "{{response}}",
                                    &format!("{{{{{}_response}}}}", pred_id),
                                )
                            } else {
                                raw_val.to_string()
                            };
                            code.push_str(&format!(
                                "        // Set {} from string template\n",
                                key
                            ));
                            code.push_str(&format!(
                                "        let mut {}_value = \"{}\".to_string();\n",
                                key,
                                val.replace('"', "\\\"")
                            ));
                            code.push_str("        for (k, v) in ctx.state.iter() {\n");
                            code.push_str(
                                "            let pattern = format!(\"{{{{{}}}}}\", k);\n",
                            );
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
                        } else {
                            code.push_str(&format!(
                                "        output = output.with_update(\"{}\", json!({}));\n",
                                key, var.value
                            ));
                        }
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
                    code.push_str(
                        "        let mut matched_branches: Vec<String> = Vec::new();\n\n",
                    );

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
                    let source = config
                        .for_each
                        .as_ref()
                        .map(|f| f.source_array.as_str())
                        .unwrap_or("items");
                    let item_var =
                        config.for_each.as_ref().map(|f| f.item_var.as_str()).unwrap_or("item");
                    let index_var =
                        config.for_each.as_ref().map(|f| f.index_var.as_str()).unwrap_or("index");
                    let counter_key = format!("{}_loop_index", node_id);
                    let done_key = format!("{}_loop_done", node_id);

                    code.push_str(&format!("        // forEach: iterate over '{}'\n", source));
                    code.push_str(&format!("        let source_arr = ctx.state.get(\"{}\").and_then(|v| v.as_array()).cloned().unwrap_or_default();\n", source));
                    code.push_str(&format!("        let idx = ctx.state.get(\"{}\").and_then(|v| v.as_u64()).unwrap_or(0) as usize;\n", counter_key));
                    code.push_str("        if idx < source_arr.len() {\n");
                    code.push_str("            let current_item = source_arr[idx].clone();\n");
                    code.push_str(&format!(
                        "            output = output.with_update(\"{}\", current_item);\n",
                        item_var
                    ));
                    code.push_str(&format!(
                        "            output = output.with_update(\"{}\", json!(idx));\n",
                        index_var
                    ));
                    code.push_str(&format!(
                        "            output = output.with_update(\"{}\", json!(idx + 1));\n",
                        counter_key
                    ));
                    code.push_str(&format!(
                        "            output = output.with_update(\"{}\", json!(false));\n",
                        done_key
                    ));

                    // Collect results
                    if config.results.collect {
                        let agg_key =
                            config.results.aggregation_key.as_deref().unwrap_or("loop_results");
                        code.push_str("            // Collect previous iteration result if any\n");
                        code.push_str(&format!("            let mut results = ctx.state.get(\"{}\").and_then(|v| v.as_array()).cloned().unwrap_or_default();\n", agg_key));
                        code.push_str("            if idx > 0 {\n");
                        code.push_str(
                            "                // Capture the response from the previous iteration\n",
                        );
                        code.push_str(
                            "                if let Some(resp) = ctx.state.get(\"response\") {\n",
                        );
                        code.push_str("                    results.push(resp.clone());\n");
                        code.push_str("                }\n");
                        code.push_str("            }\n");
                        code.push_str(&format!(
                            "            output = output.with_update(\"{}\", json!(results));\n",
                            agg_key
                        ));
                    }

                    code.push_str("        } else {\n");
                    code.push_str(&format!(
                        "            output = output.with_update(\"{}\", json!(true));\n",
                        done_key
                    ));

                    // Final collection on completion
                    if config.results.collect {
                        let agg_key =
                            config.results.aggregation_key.as_deref().unwrap_or("loop_results");
                        code.push_str(&format!("            let mut results = ctx.state.get(\"{}\").and_then(|v| v.as_array()).cloned().unwrap_or_default();\n", agg_key));
                        code.push_str(
                            "            if let Some(resp) = ctx.state.get(\"response\") {\n",
                        );
                        code.push_str("                results.push(resp.clone());\n");
                        code.push_str("            }\n");
                        code.push_str(&format!(
                            "            output = output.with_update(\"{}\", json!(results));\n",
                            agg_key
                        ));
                    }

                    code.push_str("        }\n");
                }
                crate::codegen::action_nodes::LoopType::Times => {
                    let count = config
                        .times
                        .as_ref()
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
                    code.push_str(
                        "            output = output.with_update(\"index\", json!(idx));\n",
                    );
                    code.push_str(&format!(
                        "            output = output.with_update(\"{}\", json!(idx + 1));\n",
                        counter_key
                    ));
                    code.push_str(&format!(
                        "            output = output.with_update(\"{}\", json!(false));\n",
                        done_key
                    ));

                    if config.results.collect {
                        let agg_key =
                            config.results.aggregation_key.as_deref().unwrap_or("loop_results");
                        code.push_str(&format!("            let mut results = ctx.state.get(\"{}\").and_then(|v| v.as_array()).cloned().unwrap_or_default();\n", agg_key));
                        code.push_str("            if idx > 0 {\n");
                        code.push_str(
                            "                if let Some(resp) = ctx.state.get(\"response\") {\n",
                        );
                        code.push_str("                    results.push(resp.clone());\n");
                        code.push_str("                }\n");
                        code.push_str("            }\n");
                        code.push_str(&format!(
                            "            output = output.with_update(\"{}\", json!(results));\n",
                            agg_key
                        ));
                    }

                    code.push_str("        } else {\n");
                    code.push_str(&format!(
                        "            output = output.with_update(\"{}\", json!(true));\n",
                        done_key
                    ));

                    if config.results.collect {
                        let agg_key =
                            config.results.aggregation_key.as_deref().unwrap_or("loop_results");
                        code.push_str(&format!("            let mut results = ctx.state.get(\"{}\").and_then(|v| v.as_array()).cloned().unwrap_or_default();\n", agg_key));
                        code.push_str(
                            "            if let Some(resp) = ctx.state.get(\"response\") {\n",
                        );
                        code.push_str("                results.push(resp.clone());\n");
                        code.push_str("            }\n");
                        code.push_str(&format!(
                            "            output = output.with_update(\"{}\", json!(results));\n",
                            agg_key
                        ));
                    }

                    code.push_str("        }\n");
                }
                crate::codegen::action_nodes::LoopType::While => {
                    let condition_field = config
                        .while_config
                        .as_ref()
                        .map(|w| w.condition.as_str())
                        .unwrap_or("should_continue");
                    let counter_key = format!("{}_loop_index", node_id);
                    let done_key = format!("{}_loop_done", node_id);

                    code.push_str(&format!(
                        "        // while: loop while '{}' is truthy\n",
                        condition_field
                    ));
                    code.push_str(&format!("        let idx = ctx.state.get(\"{}\").and_then(|v| v.as_u64()).unwrap_or(0) as usize;\n", counter_key));
                    code.push_str(&format!(
                        "        let cond_val = ctx.state.get(\"{}\");\n",
                        condition_field
                    ));
                    code.push_str("        let should_continue = match cond_val {\n");
                    code.push_str(
                        "            Some(v) if v.is_boolean() => v.as_bool().unwrap_or(false),\n",
                    );
                    code.push_str("            Some(v) if v.is_string() => !v.as_str().unwrap_or(\"\").is_empty() && v.as_str() != Some(\"false\"),\n");
                    code.push_str("            Some(v) if v.is_number() => v.as_f64().unwrap_or(0.0) != 0.0,\n");
                    code.push_str("            Some(v) if v.is_null() => false,\n");
                    code.push_str("            Some(_) => true,\n");
                    code.push_str("            None => false,\n");
                    code.push_str("        };\n");
                    code.push_str("        if should_continue {\n");
                    code.push_str(
                        "            output = output.with_update(\"index\", json!(idx));\n",
                    );
                    code.push_str(&format!(
                        "            output = output.with_update(\"{}\", json!(idx + 1));\n",
                        counter_key
                    ));
                    code.push_str(&format!(
                        "            output = output.with_update(\"{}\", json!(false));\n",
                        done_key
                    ));
                    code.push_str("        } else {\n");
                    code.push_str(&format!(
                        "            output = output.with_update(\"{}\", json!(true));\n",
                        done_key
                    ));
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
                    code.push_str(
                        "        let mut results: Vec<serde_json::Value> = Vec::new();\n",
                    );
                    if branch_keys.is_empty() {
                        // Collect all non-system state values
                        code.push_str("        for (k, v) in ctx.state.iter() {\n");
                        code.push_str(
                            "            if k != \"message\" && k != \"classification\" {\n",
                        );
                        code.push_str("                results.push(v.clone());\n");
                        code.push_str("            }\n");
                        code.push_str("        }\n");
                    } else {
                        for key in branch_keys {
                            code.push_str(&format!(
                                "        if let Some(v) = ctx.state.get(\"{}\") {{\n",
                                key
                            ));
                            code.push_str("            results.push(v.clone());\n");
                            code.push_str("        }\n");
                        }
                    }
                    code.push_str(&format!(
                        "        output = output.with_update(\"{}\", json!(results));\n",
                        output_key
                    ));
                }
                crate::codegen::action_nodes::CombineStrategy::Object => {
                    code.push_str("        // Combine strategy: object — merge branch outputs into an object\n");
                    code.push_str("        let mut merged = serde_json::Map::new();\n");
                    if branch_keys.is_empty() {
                        code.push_str("        for (k, v) in ctx.state.iter() {\n");
                        code.push_str(
                            "            if k != \"message\" && k != \"classification\" {\n",
                        );
                        code.push_str("                merged.insert(k.clone(), v.clone());\n");
                        code.push_str("            }\n");
                        code.push_str("        }\n");
                    } else {
                        for key in branch_keys {
                            code.push_str(&format!(
                                "        if let Some(v) = ctx.state.get(\"{}\") {{\n",
                                key
                            ));
                            code.push_str(&format!(
                                "            merged.insert(\"{}\".to_string(), v.clone());\n",
                                key
                            ));
                            code.push_str("        }\n");
                        }
                    }
                    code.push_str(&format!(
                        "        output = output.with_update(\"{}\", json!(merged));\n",
                        output_key
                    ));
                }
                crate::codegen::action_nodes::CombineStrategy::First => {
                    code.push_str(
                        "        // Combine strategy: first — use first available branch output\n",
                    );
                    if branch_keys.is_empty() {
                        code.push_str("        let first_val = ctx.state.get(\"response\").cloned().unwrap_or(json!(null));\n");
                    } else {
                        code.push_str("        let first_val = None\n");
                        for key in branch_keys {
                            code.push_str(&format!(
                                "            .or_else(|| ctx.state.get(\"{}\").cloned())\n",
                                key
                            ));
                        }
                        code.push_str("            .unwrap_or(json!(null));\n");
                    }
                    code.push_str(&format!(
                        "        output = output.with_update(\"{}\", first_val);\n",
                        output_key
                    ));
                }
                crate::codegen::action_nodes::CombineStrategy::Last => {
                    code.push_str(
                        "        // Combine strategy: last — use last available branch output\n",
                    );
                    if branch_keys.is_empty() {
                        code.push_str("        let last_val = ctx.state.get(\"response\").cloned().unwrap_or(json!(null));\n");
                    } else {
                        code.push_str("        let last_val = None\n");
                        for key in branch_keys.iter().rev() {
                            code.push_str(&format!(
                                "            .or_else(|| ctx.state.get(\"{}\").cloned())\n",
                                key
                            ));
                        }
                        code.push_str("            .unwrap_or(json!(null));\n");
                    }
                    code.push_str(&format!(
                        "        output = output.with_update(\"{}\", last_val);\n",
                        output_key
                    ));
                }
            }

            code.push_str("        Ok(output)\n");
            code.push_str("    });\n\n");
        }
        ActionNodeConfig::Http(config) => {
            code.push_str(&format!("    // Action Node: {} (HTTP)\n", config.standard.name));
            code.push_str(&format!("    let {}_node = adk_graph::node::FunctionNode::new(\"{}\", |ctx| async move {{\n", node_id, node_id));

            let output_key = if config.standard.mapping.output_key.is_empty() {
                "httpResult"
            } else {
                &config.standard.mapping.output_key
            };

            // Build URL with variable interpolation
            let raw_url = &config.url;
            let url = if let Some(pred_id) = parallel_predecessor {
                raw_url.replace("{{response}}", &format!("{{{{{}_response}}}}", pred_id))
            } else {
                raw_url.to_string()
            };

            code.push_str(&format!(
                "        let mut url = \"{}\".to_string();\n",
                url.replace('"', "\\\"")
            ));
            code.push_str("        for (k, v) in ctx.state.iter() {\n");
            code.push_str("            let pattern = format!(\"{{{{{}}}}}\", k);\n");
            code.push_str("            if let Some(s) = v.as_str() {\n");
            code.push_str("                url = url.replace(&pattern, s);\n");
            code.push_str("            } else {\n");
            code.push_str("                url = url.replace(&pattern, &v.to_string());\n");
            code.push_str("            }\n");
            code.push_str("        }\n\n");

            // Build the request
            let method_fn = match config.method {
                crate::codegen::action_node_types::HttpMethod::Get => "get",
                crate::codegen::action_node_types::HttpMethod::Post => "post",
                crate::codegen::action_node_types::HttpMethod::Put => "put",
                crate::codegen::action_node_types::HttpMethod::Patch => "patch",
                crate::codegen::action_node_types::HttpMethod::Delete => "delete",
            };

            code.push_str("        let client = reqwest::Client::new();\n");

            // Only make req mutable if we'll modify it (headers, auth, or body)
            let needs_mut = !config.headers.is_empty()
                || config.auth.auth_type != "none"
                || config.body.body_type != "none";
            if needs_mut {
                code.push_str(&format!("        let mut req = client.{}(&url);\n\n", method_fn));
            } else {
                code.push_str(&format!("        let req = client.{}(&url);\n\n", method_fn));
            }

            // Headers
            for (key, value) in &config.headers {
                let val = if let Some(pred_id) = parallel_predecessor {
                    value.replace("{{response}}", &format!("{{{{{}_response}}}}", pred_id))
                } else {
                    value.to_string()
                };
                if val.contains("{{") {
                    // Header value has template variables — interpolate from state
                    code.push_str(&format!(
                        "        let mut hdr_val = \"{}\".to_string();\n",
                        val.replace('"', "\\\"")
                    ));
                    code.push_str("        for (k, v) in ctx.state.iter() {\n");
                    code.push_str("            let pattern = format!(\"{{{{{}}}}}\", k);\n");
                    code.push_str("            if let Some(s) = v.as_str() {\n");
                    code.push_str("                hdr_val = hdr_val.replace(&pattern, s);\n");
                    code.push_str("            } else {\n");
                    code.push_str(
                        "                hdr_val = hdr_val.replace(&pattern, &v.to_string());\n",
                    );
                    code.push_str("            }\n");
                    code.push_str("        }\n");
                    code.push_str(&format!(
                        "        req = req.header(\"{}\", hdr_val);\n",
                        key.replace('"', "\\\"")
                    ));
                } else {
                    code.push_str(&format!(
                        "        req = req.header(\"{}\", \"{}\");\n",
                        key.replace('"', "\\\""),
                        val.replace('"', "\\\"")
                    ));
                }
            }

            // Authentication
            match config.auth.auth_type.as_str() {
                "bearer" => {
                    if let Some(bearer) = &config.auth.bearer {
                        let token = &bearer.token;
                        if token.contains("{{") {
                            code.push_str(&format!(
                                "        let mut bearer_token = \"{}\".to_string();\n",
                                token.replace('"', "\\\"")
                            ));
                            code.push_str("        for (k, v) in ctx.state.iter() {\n");
                            code.push_str(
                                "            let pattern = format!(\"{{{{{}}}}}\", k);\n",
                            );
                            code.push_str("            if let Some(s) = v.as_str() {\n");
                            code.push_str("                bearer_token = bearer_token.replace(&pattern, s);\n");
                            code.push_str("            }\n");
                            code.push_str("        }\n");
                            code.push_str("        req = req.bearer_auth(&bearer_token);\n");
                        } else {
                            code.push_str(&format!(
                                "        req = req.bearer_auth(\"{}\");\n",
                                token.replace('"', "\\\"")
                            ));
                        }
                    }
                }
                "basic" => {
                    if let Some(basic) = &config.auth.basic {
                        code.push_str(&format!(
                            "        req = req.basic_auth(\"{}\", Some(\"{}\"));\n",
                            basic.username.replace('"', "\\\""),
                            basic.password.replace('"', "\\\"")
                        ));
                    }
                }
                "api_key" => {
                    if let Some(api_key) = &config.auth.api_key {
                        let val = &api_key.value;
                        if val.contains("{{") {
                            code.push_str(&format!(
                                "        let mut api_key_val = \"{}\".to_string();\n",
                                val.replace('"', "\\\"")
                            ));
                            code.push_str("        for (k, v) in ctx.state.iter() {\n");
                            code.push_str(
                                "            let pattern = format!(\"{{{{{}}}}}\", k);\n",
                            );
                            code.push_str("            if let Some(s) = v.as_str() {\n");
                            code.push_str(
                                "                api_key_val = api_key_val.replace(&pattern, s);\n",
                            );
                            code.push_str("            }\n");
                            code.push_str("        }\n");
                            code.push_str(&format!(
                                "        req = req.header(\"{}\", api_key_val);\n",
                                api_key.header_name.replace('"', "\\\"")
                            ));
                        } else {
                            code.push_str(&format!(
                                "        req = req.header(\"{}\", \"{}\");\n",
                                api_key.header_name.replace('"', "\\\""),
                                val.replace('"', "\\\"")
                            ));
                        }
                    }
                }
                _ => {} // "none" — no auth
            }

            // Body
            match config.body.body_type.as_str() {
                "json" => {
                    if let Some(content) = &config.body.content {
                        let body_str = content.to_string();
                        if body_str.contains("{{") {
                            // Body has template variables — interpolate
                            code.push_str(&format!(
                                "        let mut body_str = r#\"{}\"#.to_string();\n",
                                body_str.replace('"', "\\\"")
                            ));
                            code.push_str("        for (k, v) in ctx.state.iter() {\n");
                            code.push_str(
                                "            let pattern = format!(\"{{{{{}}}}}\", k);\n",
                            );
                            code.push_str("            if let Some(s) = v.as_str() {\n");
                            code.push_str(
                                "                body_str = body_str.replace(&pattern, s);\n",
                            );
                            code.push_str("            } else {\n");
                            code.push_str("                body_str = body_str.replace(&pattern, &v.to_string());\n");
                            code.push_str("            }\n");
                            code.push_str("        }\n");
                            code.push_str("        let body_json: serde_json::Value = serde_json::from_str(&body_str).unwrap_or(json!(body_str));\n");
                            code.push_str("        req = req.json(&body_json);\n");
                        } else {
                            code.push_str(&format!(
                                "        req = req.json(&json!({}));\n",
                                body_str
                            ));
                        }
                    }
                }
                "form" => {
                    if let Some(content) = &config.body.content {
                        code.push_str(&format!("        req = req.form(&json!({}));\n", content));
                    }
                }
                "raw" => {
                    if let Some(content) = &config.body.content {
                        let raw = content
                            .as_str()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| content.to_string());
                        code.push_str(&format!(
                            "        req = req.body(\"{}\");\n",
                            raw.replace('"', "\\\"")
                        ));
                    }
                }
                _ => {} // "none" — no body
            }

            // Send request and handle response
            code.push_str("\n        let resp = req.send().await;\n");
            code.push_str("        match resp {\n");
            code.push_str("            Ok(response) => {\n");
            code.push_str("                let status = response.status().as_u16();\n");

            // Status validation
            if let Some(validation) = &config.response.status_validation {
                if validation.contains('-') {
                    let parts: Vec<&str> = validation.split('-').collect();
                    if parts.len() == 2 {
                        code.push_str(&format!(
                            "                if status < {} || status > {} {{\n",
                            parts[0].trim(),
                            parts[1].trim()
                        ));
                        code.push_str("                    let body = response.text().await.unwrap_or_default();\n");
                        code.push_str(&format!(
                            "                    return Ok(NodeOutput::new().with_update(\"{}\", json!({{\n",
                            output_key
                        ));
                        code.push_str("                        \"error\": true, \"status\": status, \"body\": body\n");
                        code.push_str("                    })));\n");
                        code.push_str("                }\n");
                    }
                }
            }

            // Parse response based on type
            match config.response.response_type.as_str() {
                "text" => {
                    code.push_str(
                        "                let body = response.text().await.unwrap_or_default();\n",
                    );
                    if let Some(json_path) = &config.response.json_path {
                        // Even for text, if jsonPath is set, try to parse and extract
                        code.push_str("                let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or(json!(body));\n");
                        code.push_str(&format!(
                            "                // JSONPath extraction: {}\n",
                            json_path
                        ));
                        generate_json_path_extraction(&mut code, json_path, output_key);
                    } else {
                        code.push_str(&format!(
                            "                Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"status\": status, \"body\": body }})))\n",
                            output_key
                        ));
                    }
                }
                _ => {
                    // Default: JSON — try JSON first, fall back to text
                    code.push_str(
                        "                let text = response.text().await.unwrap_or_default();\n",
                    );
                    code.push_str("                let body: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|_| json!(text));\n");
                    if let Some(json_path) = &config.response.json_path {
                        code.push_str(&format!(
                            "                // JSONPath extraction: {}\n",
                            json_path
                        ));
                        generate_json_path_extraction(&mut code, json_path, output_key);
                    } else {
                        code.push_str(&format!(
                            "                Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"status\": status, \"body\": body }})))\n",
                            output_key
                        ));
                    }
                }
            }

            code.push_str("            }\n");
            code.push_str("            Err(e) => {\n");
            code.push_str(&format!(
                "                Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"error\": true, \"message\": e.to_string() }})))\n",
                output_key
            ));
            code.push_str("            }\n");
            code.push_str("        }\n");
            code.push_str("    });\n\n");
        }
        ActionNodeConfig::Wait(config) => {
            code.push_str(&format!("    // Action Node: {} (Wait)\n", config.standard.name));
            code.push_str(&format!("    let {}_node = adk_graph::node::FunctionNode::new(\"{}\", |_ctx| async move {{\n", node_id, node_id));

            let output_key = if config.standard.mapping.output_key.is_empty() {
                "wait_result"
            } else {
                &config.standard.mapping.output_key
            };

            match config.wait_type {
                crate::codegen::action_nodes::WaitType::Fixed => {
                    if let Some(fixed) = &config.fixed {
                        let ms = match fixed.unit.as_str() {
                            "ms" => fixed.duration,
                            "s" => fixed.duration * 1000,
                            "m" => fixed.duration * 60 * 1000,
                            "h" => fixed.duration * 60 * 60 * 1000,
                            _ => fixed.duration,
                        };
                        code.push_str(&format!(
                            "        // Fixed wait: {} {}\n",
                            fixed.duration, fixed.unit
                        ));
                        code.push_str(&format!("        tokio::time::sleep(std::time::Duration::from_millis({})).await;\n", ms));
                        code.push_str(&format!("        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"waited\": true, \"duration_ms\": {} }})))\n", output_key, ms));
                    } else {
                        // No fixed config — default 1 second
                        code.push_str("        // Fixed wait: default 1s\n");
                        code.push_str("        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;\n");
                        code.push_str(&format!("        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"waited\": true, \"duration_ms\": 1000 }})))\n", output_key));
                    }
                }
                crate::codegen::action_nodes::WaitType::Until => {
                    if let Some(until) = &config.until {
                        code.push_str("        // Wait until timestamp\n");
                        code.push_str(&format!(
                            "        let target = chrono::DateTime::parse_from_rfc3339(\"{}\")\n",
                            until.timestamp
                        ));
                        code.push_str(
                            "            .unwrap_or_else(|_| chrono::Utc::now().fixed_offset());\n",
                        );
                        code.push_str("        let now = chrono::Utc::now();\n");
                        code.push_str("        if target > now {\n");
                        code.push_str("            let duration = (target - now).to_std().unwrap_or_default();\n");
                        code.push_str("            tokio::time::sleep(duration).await;\n");
                        code.push_str("        }\n");
                        code.push_str(&format!("        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"waited\": true }})))\n", output_key));
                    } else {
                        code.push_str(&format!("        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"waited\": false, \"reason\": \"no timestamp configured\" }})))\n", output_key));
                    }
                }
                crate::codegen::action_nodes::WaitType::Condition => {
                    if let Some(condition) = &config.condition {
                        code.push_str("        // Poll until condition is met\n");
                        code.push_str(&format!(
                            "        let poll_interval = std::time::Duration::from_millis({});\n",
                            condition.poll_interval
                        ));
                        code.push_str(&format!(
                            "        let max_wait = std::time::Duration::from_millis({});\n",
                            condition.max_wait
                        ));
                        code.push_str("        let start = std::time::Instant::now();\n");
                        code.push_str("        loop {\n");
                        code.push_str("            if start.elapsed() >= max_wait {\n");
                        code.push_str(&format!("                break Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"waited\": true, \"timed_out\": true }})));\n", output_key));
                        code.push_str("            }\n");
                        code.push_str("            tokio::time::sleep(poll_interval).await;\n");
                        code.push_str("        }\n");
                    } else {
                        code.push_str(&format!("        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"waited\": false }})))\n", output_key));
                    }
                }
                crate::codegen::action_nodes::WaitType::Webhook => {
                    // Webhook wait is complex — generate a simple timeout-based placeholder
                    if let Some(webhook) = &config.webhook {
                        code.push_str("        // Webhook wait (simplified): sleep for timeout as placeholder\n");
                        code.push_str(&format!("        tokio::time::sleep(std::time::Duration::from_millis({})).await;\n", webhook.timeout.min(30000)));
                        code.push_str(&format!("        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"waited\": true, \"webhook\": \"{}\" }})))\n", output_key, webhook.path.replace('"', "\\\"")));
                    } else {
                        code.push_str(&format!("        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"waited\": false }})))\n", output_key));
                    }
                }
            }

            code.push_str("    });\n\n");
        }
        ActionNodeConfig::Database(config) => {
            code.push_str(&format!(
                "    // Action Node: {} (Database - {:?})\n",
                config.standard.name, config.db_type
            ));
            code.push_str(&format!("    let {}_node = adk_graph::node::FunctionNode::new(\"{}\", |ctx| async move {{\n", node_id, node_id));

            let output_key = if config.standard.mapping.output_key.is_empty() {
                "dbResult"
            } else {
                &config.standard.mapping.output_key
            };

            // Connection string with variable interpolation
            let conn_str = &config.connection.connection_string;
            if conn_str.contains("{{") {
                code.push_str(&format!(
                    "        let mut conn_str = \"{}\".to_string();\n",
                    conn_str.replace('"', "\\\"")
                ));
                code.push_str("        for (k, v) in ctx.state.iter() {\n");
                code.push_str("            let pattern = format!(\"{{{{{}}}}}\", k);\n");
                code.push_str("            if let Some(s) = v.as_str() {\n");
                code.push_str("                conn_str = conn_str.replace(&pattern, s);\n");
                code.push_str("            } else {\n");
                code.push_str(
                    "                conn_str = conn_str.replace(&pattern, &v.to_string());\n",
                );
                code.push_str("            }\n");
                code.push_str("        }\n");
            } else {
                code.push_str(&format!(
                    "        let conn_str = \"{}\".to_string();\n",
                    conn_str.replace('"', "\\\"")
                ));
            }

            // Also support credential_ref — override conn_str from state if ref is set
            if let Some(cred_ref) = &config.connection.credential_ref {
                if !cred_ref.is_empty() {
                    code.push_str(&format!(
                        "        let conn_str = ctx.state.get(\"{}\").and_then(|v| v.as_str()).map(|s| s.to_string()).unwrap_or(conn_str);\n",
                        cred_ref
                    ));
                }
            }

            match config.db_type {
                crate::codegen::action_node_types::DatabaseType::Postgresql
                | crate::codegen::action_node_types::DatabaseType::Mysql
                | crate::codegen::action_node_types::DatabaseType::Sqlite => {
                    // SQL databases via sqlx
                    if let Some(sql) = &config.sql {
                        let query = &sql.query;
                        // Interpolate {{variables}} in the query
                        if query.contains("{{") {
                            code.push_str(&format!(
                                "        let mut query_str = \"{}\".to_string();\n",
                                query.replace('"', "\\\"")
                            ));
                            code.push_str("        for (k, v) in ctx.state.iter() {\n");
                            code.push_str(
                                "            let pattern = format!(\"{{{{{}}}}}\", k);\n",
                            );
                            code.push_str("            if let Some(s) = v.as_str() {\n");
                            code.push_str(
                                "                query_str = query_str.replace(&pattern, s);\n",
                            );
                            code.push_str("            } else {\n");
                            code.push_str("                query_str = query_str.replace(&pattern, &v.to_string());\n");
                            code.push_str("            }\n");
                            code.push_str("        }\n");
                        } else {
                            code.push_str(&format!(
                                "        let query_str = \"{}\".to_string();\n",
                                query.replace('"', "\\\"")
                            ));
                        }

                        match config.db_type {
                            crate::codegen::action_node_types::DatabaseType::Sqlite => {
                                code.push_str("        let pool = sqlx::SqlitePool::connect(&conn_str).await\n");
                                code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"db\".into(), message: format!(\"SQLite connection failed: {}\", e) })?;\n");
                            }
                            crate::codegen::action_node_types::DatabaseType::Mysql => {
                                code.push_str("        let pool = sqlx::MySqlPool::connect(&conn_str).await\n");
                                code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"db\".into(), message: format!(\"MySQL connection failed: {}\", e) })?;\n");
                            }
                            _ => {
                                // PostgreSQL (default)
                                code.push_str(
                                    "        let pool = sqlx::PgPool::connect(&conn_str).await\n",
                                );
                                code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"db\".into(), message: format!(\"PostgreSQL connection failed: {}\", e) })?;\n");
                            }
                        }

                        match sql.operation.as_str() {
                            "query" => {
                                // SELECT — returns rows as JSON array
                                // Use database-specific row type for proper type inference
                                code.push_str("        use sqlx::Row;\n");
                                code.push_str("        use sqlx::Column;\n");
                                code.push_str("        let raw_rows = sqlx::query(&query_str)\n");
                                code.push_str("            .fetch_all(&pool).await\n");
                                code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"db\".into(), message: format!(\"Query failed: {}\", e) })?;\n");
                                code.push_str("        let rows: Vec<serde_json::Value> = raw_rows.iter().map(|row| {\n");
                                code.push_str(
                                    "            let mut obj = serde_json::Map::new();\n",
                                );
                                code.push_str("            for col in row.columns() {\n");
                                code.push_str(
                                    "                let name = col.name().to_string();\n",
                                );
                                code.push_str("                let val: serde_json::Value = row.try_get::<String, _>(col.name())\n");
                                code.push_str("                    .map(|s| json!(s))\n");
                                code.push_str("                    .or_else(|_| row.try_get::<i64, _>(col.name()).map(|n| json!(n)))\n");
                                code.push_str("                    .or_else(|_| row.try_get::<f64, _>(col.name()).map(|n| json!(n)))\n");
                                code.push_str("                    .or_else(|_| row.try_get::<bool, _>(col.name()).map(|b| json!(b)))\n");
                                code.push_str("                    .unwrap_or(json!(null));\n");
                                code.push_str("                obj.insert(name, val);\n");
                                code.push_str("            }\n");
                                code.push_str("            serde_json::Value::Object(obj)\n");
                                code.push_str("        }).collect();\n");
                                code.push_str("        let row_count = rows.len();\n");
                                code.push_str(&format!(
                                    "        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"rows\": rows, \"count\": row_count }})))\n",
                                    output_key
                                ));
                            }
                            _ => {
                                // INSERT, UPDATE, DELETE, UPSERT — returns affected rows
                                code.push_str("        let result = sqlx::query(&query_str)\n");
                                code.push_str("            .execute(&pool).await\n");
                                code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"db\".into(), message: format!(\"Query failed: {}\", e) })?;\n");
                                code.push_str(&format!(
                                    "        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"affected\": result.rows_affected(), \"operation\": \"{}\" }})))\n",
                                    output_key, sql.operation
                                ));
                            }
                        }
                    } else {
                        code.push_str(&format!(
                            "        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"error\": true, \"message\": \"No SQL query configured\" }})))\n",
                            output_key
                        ));
                    }
                }
                crate::codegen::action_node_types::DatabaseType::Mongodb => {
                    if let Some(mongo) = &config.mongodb {
                        code.push_str(
                            "        let client = mongodb::Client::with_uri_str(&conn_str).await\n",
                        );
                        code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"db\".into(), message: format!(\"MongoDB connection failed: {}\", e) })?;\n");
                        // Extract database name from connection string
                        code.push_str("        let db_name = conn_str.rsplit('/').next()\n");
                        code.push_str("            .and_then(|s| s.split('?').next())\n");
                        code.push_str("            .filter(|s| !s.is_empty())\n");
                        code.push_str("            .unwrap_or(\"test\");\n");
                        code.push_str("        let db = client.database(db_name);\n");
                        code.push_str(&format!(
                            "        let collection = db.collection::<mongodb::bson::Document>(\"{}\");\n",
                            mongo.collection.replace('"', "\\\"")
                        ));

                        let filter_str = mongo
                            .filter
                            .as_ref()
                            .map(|f| f.to_string())
                            .unwrap_or_else(|| "{}".to_string());

                        match mongo.operation.as_str() {
                            "find" => {
                                code.push_str(&format!(
                                    "        let filter_json: serde_json::Value = serde_json::from_str(r#\"{}\"#).unwrap_or(json!({{}}));\n",
                                    filter_str.replace('"', "\\\"")
                                ));
                                code.push_str("        let filter_doc = mongodb::bson::to_document(&filter_json).unwrap_or_default();\n");
                                code.push_str("        use futures::TryStreamExt;\n");
                                code.push_str(
                                    "        let mut cursor = collection.find(filter_doc).await\n",
                                );
                                code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"db\".into(), message: format!(\"MongoDB find failed: {}\", e) })?;\n");
                                code.push_str(
                                    "        let mut docs: Vec<serde_json::Value> = Vec::new();\n",
                                );
                                code.push_str(
                                    "        while let Some(doc) = cursor.try_next().await\n",
                                );
                                code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"db\".into(), message: format!(\"Cursor error: {}\", e) })? {\n");
                                code.push_str("            if let Ok(json) = mongodb::bson::from_document::<serde_json::Value>(doc) {\n");
                                code.push_str("                docs.push(json);\n");
                                code.push_str("            }\n");
                                code.push_str("        }\n");
                                code.push_str(&format!(
                                    "        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"docs\": docs, \"count\": docs.len() }})))\n",
                                    output_key
                                ));
                            }
                            "findOne" => {
                                code.push_str(&format!(
                                    "        let filter_json: serde_json::Value = serde_json::from_str(r#\"{}\"#).unwrap_or(json!({{}}));\n",
                                    filter_str.replace('"', "\\\"")
                                ));
                                code.push_str("        let filter_doc = mongodb::bson::to_document(&filter_json).unwrap_or_default();\n");
                                code.push_str(
                                    "        let result = collection.find_one(filter_doc).await\n",
                                );
                                code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"db\".into(), message: format!(\"MongoDB findOne failed: {}\", e) })?;\n");
                                code.push_str("        let doc_json = result.map(|d| mongodb::bson::from_document::<serde_json::Value>(d).unwrap_or(json!(null))).unwrap_or(json!(null));\n");
                                code.push_str(&format!(
                                    "        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"doc\": doc_json }})))\n",
                                    output_key
                                ));
                            }
                            "insert" => {
                                let doc_str = mongo
                                    .document
                                    .as_ref()
                                    .map(|d| d.to_string())
                                    .unwrap_or_else(|| "{}".to_string());
                                code.push_str(&format!(
                                    "        let doc_json: serde_json::Value = serde_json::from_str(r#\"{}\"#).unwrap_or(json!({{}}));\n",
                                    doc_str.replace('"', "\\\"")
                                ));
                                code.push_str("        let doc = mongodb::bson::to_document(&doc_json).unwrap_or_default();\n");
                                code.push_str(
                                    "        let result = collection.insert_one(doc).await\n",
                                );
                                code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"db\".into(), message: format!(\"MongoDB insert failed: {}\", e) })?;\n");
                                code.push_str(&format!(
                                    "        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"inserted_id\": result.inserted_id.to_string() }})))\n",
                                    output_key
                                ));
                            }
                            "update" => {
                                code.push_str(&format!(
                                    "        let filter_json: serde_json::Value = serde_json::from_str(r#\"{}\"#).unwrap_or(json!({{}}));\n",
                                    filter_str.replace('"', "\\\"")
                                ));
                                code.push_str("        let filter_doc = mongodb::bson::to_document(&filter_json).unwrap_or_default();\n");
                                let doc_str = mongo
                                    .document
                                    .as_ref()
                                    .map(|d| d.to_string())
                                    .unwrap_or_else(|| "{}".to_string());
                                code.push_str(&format!(
                                    "        let update_json: serde_json::Value = serde_json::from_str(r#\"{}\"#).unwrap_or(json!({{}}));\n",
                                    doc_str.replace('"', "\\\"")
                                ));
                                code.push_str("        let update_doc = mongodb::bson::to_document(&update_json).unwrap_or_default();\n");
                                code.push_str("        let result = collection.update_many(filter_doc, update_doc).await\n");
                                code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"db\".into(), message: format!(\"MongoDB update failed: {}\", e) })?;\n");
                                code.push_str(&format!(
                                    "        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"matched\": result.matched_count, \"modified\": result.modified_count }})))\n",
                                    output_key
                                ));
                            }
                            "delete" => {
                                code.push_str(&format!(
                                    "        let filter_json: serde_json::Value = serde_json::from_str(r#\"{}\"#).unwrap_or(json!({{}}));\n",
                                    filter_str.replace('"', "\\\"")
                                ));
                                code.push_str("        let filter_doc = mongodb::bson::to_document(&filter_json).unwrap_or_default();\n");
                                code.push_str("        let result = collection.delete_many(filter_doc).await\n");
                                code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"db\".into(), message: format!(\"MongoDB delete failed: {}\", e) })?;\n");
                                code.push_str(&format!(
                                    "        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"deleted\": result.deleted_count }})))\n",
                                    output_key
                                ));
                            }
                            _ => {
                                code.push_str(&format!(
                                    "        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"error\": true, \"message\": \"Unknown MongoDB operation\" }})))\n",
                                    output_key
                                ));
                            }
                        }
                    } else {
                        code.push_str(&format!(
                            "        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"error\": true, \"message\": \"No MongoDB config\" }})))\n",
                            output_key
                        ));
                    }
                }
                crate::codegen::action_node_types::DatabaseType::Redis => {
                    if let Some(redis_cfg) = &config.redis {
                        code.push_str(
                            "        let client = redis::Client::open(conn_str.as_str())\n",
                        );
                        code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"db\".into(), message: format!(\"Redis connection failed: {}\", e) })?;\n");
                        code.push_str("        let mut con = client.get_multiplexed_async_connection().await\n");
                        code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"db\".into(), message: format!(\"Redis connect failed: {}\", e) })?;\n");

                        // Key with variable interpolation
                        let key = &redis_cfg.key;
                        if key.contains("{{") {
                            code.push_str(&format!(
                                "        let mut redis_key = \"{}\".to_string();\n",
                                key.replace('"', "\\\"")
                            ));
                            code.push_str("        for (k, v) in ctx.state.iter() {\n");
                            code.push_str(
                                "            let pattern = format!(\"{{{{{}}}}}\", k);\n",
                            );
                            code.push_str("            if let Some(s) = v.as_str() {\n");
                            code.push_str(
                                "                redis_key = redis_key.replace(&pattern, s);\n",
                            );
                            code.push_str("            }\n");
                            code.push_str("        }\n");
                        } else {
                            code.push_str(&format!(
                                "        let redis_key = \"{}\".to_string();\n",
                                key.replace('"', "\\\"")
                            ));
                        }

                        code.push_str("        use redis::AsyncCommands;\n");

                        match redis_cfg.operation.as_str() {
                            "get" => {
                                code.push_str(
                                    "        let val: Option<String> = con.get(&redis_key).await\n",
                                );
                                code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"db\".into(), message: format!(\"Redis GET failed: {}\", e) })?;\n");
                                code.push_str(&format!(
                                    "        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"value\": val }})))\n",
                                    output_key
                                ));
                            }
                            "set" => {
                                let val_str = redis_cfg
                                    .value
                                    .as_ref()
                                    .map(|v| match v {
                                        serde_json::Value::String(s) => s.clone(),
                                        other => other.to_string(),
                                    })
                                    .unwrap_or_default();
                                code.push_str(&format!(
                                    "        let val = \"{}\";\n",
                                    val_str.replace('"', "\\\"")
                                ));
                                if let Some(ttl) = redis_cfg.ttl {
                                    code.push_str(&format!(
                                        "        let _: () = con.set_ex(&redis_key, val, {}).await\n",
                                        ttl
                                    ));
                                } else {
                                    code.push_str(
                                        "        let _: () = con.set(&redis_key, val).await\n",
                                    );
                                }
                                code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"db\".into(), message: format!(\"Redis SET failed: {}\", e) })?;\n");
                                code.push_str(&format!(
                                    "        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"set\": true, \"key\": redis_key }})))\n",
                                    output_key
                                ));
                            }
                            "del" => {
                                code.push_str(
                                    "        let deleted: i64 = con.del(&redis_key).await\n",
                                );
                                code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"db\".into(), message: format!(\"Redis DEL failed: {}\", e) })?;\n");
                                code.push_str(&format!(
                                    "        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"deleted\": deleted }})))\n",
                                    output_key
                                ));
                            }
                            "hget" => {
                                // For HGET, use value as field name
                                let field = redis_cfg
                                    .value
                                    .as_ref()
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("field");
                                code.push_str(&format!(
                                    "        let val: Option<String> = con.hget(&redis_key, \"{}\").await\n",
                                    field.replace('"', "\\\"")
                                ));
                                code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"db\".into(), message: format!(\"Redis HGET failed: {}\", e) })?;\n");
                                code.push_str(&format!(
                                    "        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"value\": val }})))\n",
                                    output_key
                                ));
                            }
                            "hset" => {
                                let val_str = redis_cfg
                                    .value
                                    .as_ref()
                                    .map(|v| v.to_string())
                                    .unwrap_or_else(|| "{}".to_string());
                                code.push_str(&format!(
                                    "        let fields: serde_json::Value = serde_json::from_str(r#\"{}\"#).unwrap_or(json!({{}}));\n",
                                    val_str.replace('"', "\\\"")
                                ));
                                code.push_str("        if let Some(obj) = fields.as_object() {\n");
                                code.push_str("            for (field, val) in obj {\n");
                                code.push_str("                let val_str = val.as_str().map(|s| s.to_string()).unwrap_or_else(|| val.to_string());\n");
                                code.push_str("                let _: () = con.hset(&redis_key, field, &val_str).await\n");
                                code.push_str("                    .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"db\".into(), message: format!(\"Redis HSET failed: {}\", e) })?;\n");
                                code.push_str("            }\n");
                                code.push_str("        }\n");
                                code.push_str(&format!(
                                    "        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"set\": true, \"key\": redis_key }})))\n",
                                    output_key
                                ));
                            }
                            "lpush" => {
                                let val_str = redis_cfg
                                    .value
                                    .as_ref()
                                    .map(|v| match v {
                                        serde_json::Value::String(s) => s.clone(),
                                        other => other.to_string(),
                                    })
                                    .unwrap_or_default();
                                code.push_str(&format!(
                                    "        let len: i64 = con.lpush(&redis_key, \"{}\").await\n",
                                    val_str.replace('"', "\\\"")
                                ));
                                code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"db\".into(), message: format!(\"Redis LPUSH failed: {}\", e) })?;\n");
                                code.push_str(&format!(
                                    "        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"length\": len }})))\n",
                                    output_key
                                ));
                            }
                            "rpop" => {
                                code.push_str("        let val: Option<String> = con.rpop(&redis_key, None).await\n");
                                code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"db\".into(), message: format!(\"Redis RPOP failed: {}\", e) })?;\n");
                                code.push_str(&format!(
                                    "        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"value\": val }})))\n",
                                    output_key
                                ));
                            }
                            _ => {
                                code.push_str(&format!(
                                    "        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"error\": true, \"message\": \"Unknown Redis operation\" }})))\n",
                                    output_key
                                ));
                            }
                        }
                    } else {
                        code.push_str(&format!(
                            "        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"error\": true, \"message\": \"No Redis config\" }})))\n",
                            output_key
                        ));
                    }
                }
            }

            code.push_str("    });\n\n");
        }
        ActionNodeConfig::Email(config) => {
            code.push_str(&format!(
                "    // Action Node: {} (Email - {:?})\n",
                config.standard.name, config.mode
            ));
            code.push_str(&format!("    let {}_node = adk_graph::node::FunctionNode::new(\"{}\", |ctx| async move {{\n", node_id, node_id));

            let output_key = if config.standard.mapping.output_key.is_empty() {
                "emailResult"
            } else {
                &config.standard.mapping.output_key
            };

            match config.mode {
                crate::codegen::action_node_types::EmailMode::Send => {
                    if let (Some(smtp), Some(recipients), Some(content)) =
                        (&config.smtp, &config.recipients, &config.content)
                    {
                        // Variable interpolation helper for subject and body
                        code.push_str(
                            "        // Build email content with variable interpolation\n",
                        );

                        // Subject interpolation
                        let subject = &content.subject;
                        if subject.contains("{{") {
                            code.push_str(&format!(
                                "        let mut subject = \"{}\".to_string();\n",
                                subject.replace('"', "\\\"")
                            ));
                            code.push_str("        for (k, v) in ctx.state.iter() {\n");
                            code.push_str(
                                "            let pattern = format!(\"{{{{{}}}}}\", k);\n",
                            );
                            code.push_str("            if let Some(s) = v.as_str() {\n");
                            code.push_str(
                                "                subject = subject.replace(&pattern, s);\n",
                            );
                            code.push_str("            } else {\n");
                            code.push_str("                subject = subject.replace(&pattern, &v.to_string());\n");
                            code.push_str("            }\n");
                            code.push_str("        }\n");
                        } else {
                            code.push_str(&format!(
                                "        let subject = \"{}\".to_string();\n",
                                subject.replace('"', "\\\"")
                            ));
                        }

                        // Body interpolation
                        let body = &content.body;
                        if body.contains("{{") {
                            code.push_str(&format!(
                                "        let mut body = \"{}\".to_string();\n",
                                body.replace('"', "\\\"").replace('\n', "\\n")
                            ));
                            code.push_str("        for (k, v) in ctx.state.iter() {\n");
                            code.push_str(
                                "            let pattern = format!(\"{{{{{}}}}}\", k);\n",
                            );
                            code.push_str("            if let Some(s) = v.as_str() {\n");
                            code.push_str("                body = body.replace(&pattern, s);\n");
                            code.push_str("            } else {\n");
                            code.push_str(
                                "                body = body.replace(&pattern, &v.to_string());\n",
                            );
                            code.push_str("            }\n");
                            code.push_str("        }\n");
                        } else {
                            code.push_str(&format!(
                                "        let body = \"{}\".to_string();\n",
                                body.replace('"', "\\\"").replace('\n', "\\n")
                            ));
                        }

                        // Build the email message
                        let from_addr = if let Some(ref name) = smtp.from_name {
                            format!(
                                "{} <{}>",
                                name.replace('"', "\\\""),
                                smtp.from_email.replace('"', "\\\"")
                            )
                        } else {
                            smtp.from_email.replace('"', "\\\"")
                        };

                        code.push_str(&format!(
                            "        let from = \"{}\".parse::<lettre::message::Mailbox>()\n",
                            from_addr
                        ));
                        code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"email\".into(), message: format!(\"Invalid from address: {}\", e) })?;\n");

                        // To recipients
                        code.push_str(&format!(
                            "        let to_addrs = \"{}\".to_string();\n",
                            recipients.to.replace('"', "\\\"")
                        ));

                        code.push_str(
                            "        let mut email_builder = lettre::Message::builder()\n",
                        );
                        code.push_str("            .from(from)\n");
                        code.push_str("            .subject(&subject);\n");

                        // Add To recipients (comma-separated)
                        code.push_str("        for addr in to_addrs.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {\n");
                        code.push_str("            if let Ok(mbox) = addr.parse::<lettre::message::Mailbox>() {\n");
                        code.push_str("                email_builder = email_builder.to(mbox);\n");
                        code.push_str("            }\n");
                        code.push_str("        }\n");

                        // CC recipients
                        if let Some(ref cc) = recipients.cc {
                            if !cc.is_empty() {
                                code.push_str(&format!(
                                    "        for addr in \"{}\".split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {{\n",
                                    cc.replace('"', "\\\"")
                                ));
                                code.push_str("            if let Ok(mbox) = addr.parse::<lettre::message::Mailbox>() {\n");
                                code.push_str(
                                    "                email_builder = email_builder.cc(mbox);\n",
                                );
                                code.push_str("            }\n");
                                code.push_str("        }\n");
                            }
                        }

                        // BCC recipients
                        if let Some(ref bcc) = recipients.bcc {
                            if !bcc.is_empty() {
                                code.push_str(&format!(
                                    "        for addr in \"{}\".split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {{\n",
                                    bcc.replace('"', "\\\"")
                                ));
                                code.push_str("            if let Ok(mbox) = addr.parse::<lettre::message::Mailbox>() {\n");
                                code.push_str(
                                    "                email_builder = email_builder.bcc(mbox);\n",
                                );
                                code.push_str("            }\n");
                                code.push_str("        }\n");
                            }
                        }

                        // Build message body based on type
                        match content.body_type {
                            crate::codegen::action_node_types::EmailBodyType::Html => {
                                code.push_str("        let email = email_builder\n");
                                code.push_str("            .header(lettre::message::header::ContentType::TEXT_HTML)\n");
                                code.push_str("            .body(body.clone())\n");
                                code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"email\".into(), message: format!(\"Failed to build email: {}\", e) })?;\n");
                            }
                            _ => {
                                code.push_str("        let email = email_builder\n");
                                code.push_str("            .header(lettre::message::header::ContentType::TEXT_PLAIN)\n");
                                code.push_str("            .body(body.clone())\n");
                                code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"email\".into(), message: format!(\"Failed to build email: {}\", e) })?;\n");
                            }
                        }

                        // SMTP transport — connection string with variable interpolation for password
                        let host = smtp.host.replace('"', "\\\"");
                        let username = smtp.username.replace('"', "\\\"");
                        let password = smtp.password.replace('"', "\\\"");

                        // Support password from state via {{variable}}
                        if password.contains("{{") {
                            code.push_str(&format!(
                                "        let mut smtp_password = \"{}\".to_string();\n",
                                password
                            ));
                            code.push_str("        for (k, v) in ctx.state.iter() {\n");
                            code.push_str(
                                "            let pattern = format!(\"{{{{{}}}}}\", k);\n",
                            );
                            code.push_str("            if let Some(s) = v.as_str() {\n");
                            code.push_str("                smtp_password = smtp_password.replace(&pattern, s);\n");
                            code.push_str("            }\n");
                            code.push_str("        }\n");
                        } else {
                            code.push_str(&format!(
                                "        let smtp_password = \"{}\".to_string();\n",
                                password
                            ));
                        }

                        code.push_str("        use lettre::Transport;\n");

                        if smtp.secure {
                            code.push_str(&format!(
                                "        let transport = lettre::SmtpTransport::relay(\"{}\")\n",
                                host
                            ));
                            code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"email\".into(), message: format!(\"SMTP relay failed: {}\", e) })?\n");
                            code.push_str(&format!("            .port({})\n", smtp.port));
                            code.push_str(&format!(
                                "            .credentials(lettre::transport::smtp::authentication::Credentials::new(\"{}\".to_string(), smtp_password))\n",
                                username
                            ));
                            code.push_str("            .build();\n");
                        } else {
                            code.push_str(&format!(
                                "        let transport = lettre::SmtpTransport::builder_dangerous(\"{}\")\n",
                                host
                            ));
                            code.push_str(&format!("            .port({})\n", smtp.port));
                            code.push_str(&format!(
                                "            .credentials(lettre::transport::smtp::authentication::Credentials::new(\"{}\".to_string(), smtp_password))\n",
                                username
                            ));
                            code.push_str("            .build();\n");
                        }

                        // Send the email
                        code.push_str("        match transport.send(&email) {\n");
                        code.push_str("            Ok(response) => {\n");
                        code.push_str(&format!(
                            "                Ok(NodeOutput::new().with_update(\"{}\", json!({{\n",
                            output_key
                        ));
                        code.push_str("                    \"sent\": true,\n");
                        code.push_str("                    \"message\": format!(\"Email sent successfully: {:?}\", response),\n");
                        code.push_str(&format!(
                            "                    \"to\": \"{}\",\n",
                            recipients.to.replace('"', "\\\"")
                        ));
                        code.push_str("                    \"subject\": subject\n");
                        code.push_str("                })))\n");
                        code.push_str("            }\n");
                        code.push_str("            Err(e) => {\n");
                        code.push_str(&format!(
                            "                Ok(NodeOutput::new().with_update(\"{}\", json!({{\n",
                            output_key
                        ));
                        code.push_str("                    \"sent\": false,\n");
                        code.push_str("                    \"error\": format!(\"Failed to send email: {}\", e)\n");
                        code.push_str("                })))\n");
                        code.push_str("            }\n");
                        code.push_str("        }\n");
                    } else {
                        code.push_str(&format!(
                            "        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"error\": true, \"message\": \"Email send mode requires smtp, recipients, and content configuration\" }})))\n",
                            output_key
                        ));
                    }
                }
                crate::codegen::action_node_types::EmailMode::Monitor => {
                    // IMAP monitoring — check for new emails matching filters
                    if let Some(imap_cfg) = &config.imap {
                        let host = imap_cfg.host.replace('"', "\\\"");
                        let username = imap_cfg.username.replace('"', "\\\"");
                        let password = imap_cfg.password.replace('"', "\\\"");
                        let folder = imap_cfg.folder.replace('"', "\\\"");

                        // Password interpolation
                        if password.contains("{{") {
                            code.push_str(&format!(
                                "        let mut imap_password = \"{}\".to_string();\n",
                                password
                            ));
                            code.push_str("        for (k, v) in ctx.state.iter() {\n");
                            code.push_str(
                                "            let pattern = format!(\"{{{{{}}}}}\", k);\n",
                            );
                            code.push_str("            if let Some(s) = v.as_str() {\n");
                            code.push_str("                imap_password = imap_password.replace(&pattern, s);\n");
                            code.push_str("            }\n");
                            code.push_str("        }\n");
                        } else {
                            code.push_str(&format!(
                                "        let imap_password = \"{}\".to_string();\n",
                                password
                            ));
                        }

                        // Connect to IMAP
                        if imap_cfg.secure {
                            code.push_str(
                                "        let tls = native_tls::TlsConnector::builder().build()\n",
                            );
                            code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"email\".into(), message: format!(\"TLS error: {}\", e) })?;\n");
                            code.push_str(&format!(
                                "        let client = imap::ClientBuilder::new(\"{}\", {}).native_tls(tls)\n",
                                host, imap_cfg.port
                            ));
                            code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"email\".into(), message: format!(\"IMAP connection failed: {}\", e) })?;\n");
                        } else {
                            code.push_str(&format!(
                                "        let client = imap::ClientBuilder::new(\"{}\", {}).connect()\n",
                                host, imap_cfg.port
                            ));
                            code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"email\".into(), message: format!(\"IMAP connection failed: {}\", e) })?;\n");
                        }

                        // Login
                        code.push_str(&format!(
                            "        let mut session = client.login(\"{}\", &imap_password)\n",
                            username
                        ));
                        code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"email\".into(), message: format!(\"IMAP login failed: {:?}\", e.0) })?;\n");

                        // Select folder
                        code.push_str(&format!("        session.select(\"{}\")\n", folder));
                        code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"email\".into(), message: format!(\"Folder select failed: {}\", e) })?;\n");

                        // Build search query from filters
                        let mut search_criteria = Vec::new();
                        if let Some(ref filters) = config.filters {
                            if filters.unread_only {
                                search_criteria.push("UNSEEN".to_string());
                            }
                            if let Some(ref from) = filters.from {
                                if !from.is_empty() {
                                    search_criteria.push(format!(
                                        "FROM \\\"{}\\\"",
                                        from.replace('"', "\\\\\\\"")
                                    ));
                                }
                            }
                            if let Some(ref subject) = filters.subject {
                                if !subject.is_empty() {
                                    search_criteria.push(format!(
                                        "SUBJECT \\\"{}\\\"",
                                        subject.replace('"', "\\\\\\\"")
                                    ));
                                }
                            }
                        }
                        let search_str = if search_criteria.is_empty() {
                            "ALL".to_string()
                        } else {
                            search_criteria.join(" ")
                        };

                        code.push_str(&format!(
                            "        let messages = session.search(\"{}\")\n",
                            search_str
                        ));
                        code.push_str("            .map_err(|e| adk_graph::GraphError::NodeExecutionFailed { node: \"email\".into(), message: format!(\"IMAP search failed: {}\", e) })?;\n");

                        // Fetch message details
                        code.push_str(
                            "        let mut emails: Vec<serde_json::Value> = Vec::new();\n",
                        );
                        code.push_str("        if !messages.is_empty() {\n");
                        code.push_str("            let seq_set: Vec<String> = messages.iter().map(|id| id.to_string()).collect();\n");
                        code.push_str("            let fetch_range = seq_set.join(\",\");\n");
                        code.push_str("            if let Ok(fetched) = session.fetch(&fetch_range, \"(RFC822.HEADER BODY[TEXT])\") {\n");
                        code.push_str("                for msg in fetched.iter() {\n");
                        code.push_str("                    let header = msg.header().map(|h| String::from_utf8_lossy(h).to_string()).unwrap_or_default();\n");
                        code.push_str("                    let body_text = msg.text().map(|b| String::from_utf8_lossy(b).to_string()).unwrap_or_default();\n");
                        code.push_str("                    // Parse basic headers\n");
                        code.push_str("                    let from_line = header.lines().find(|l| l.starts_with(\"From:\")).map(|l| l[5..].trim().to_string()).unwrap_or_default();\n");
                        code.push_str("                    let subject_line = header.lines().find(|l| l.starts_with(\"Subject:\")).map(|l| l[8..].trim().to_string()).unwrap_or_default();\n");
                        code.push_str("                    let date_line = header.lines().find(|l| l.starts_with(\"Date:\")).map(|l| l[5..].trim().to_string()).unwrap_or_default();\n");
                        code.push_str("                    emails.push(json!({\n");
                        code.push_str("                        \"from\": from_line,\n");
                        code.push_str("                        \"subject\": subject_line,\n");
                        code.push_str("                        \"date\": date_line,\n");
                        code.push_str("                        \"body\": body_text,\n");
                        code.push_str("                        \"uid\": msg.message\n");
                        code.push_str("                    }));\n");
                        code.push_str("                }\n");
                        code.push_str("            }\n");

                        // Mark as read if configured
                        if imap_cfg.mark_as_read {
                            code.push_str("            // Mark fetched messages as read\n");
                            code.push_str("            let _ = session.store(&fetch_range, \"+FLAGS (\\\\Seen)\");\n");
                        }

                        code.push_str("        }\n");
                        code.push_str("        let _ = session.logout();\n");

                        code.push_str("        let email_count = emails.len();\n");
                        code.push_str(&format!(
                            "        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"emails\": emails, \"count\": email_count }})))\n",
                            output_key
                        ));
                    } else {
                        code.push_str(&format!(
                            "        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"error\": true, \"message\": \"Email monitor mode requires IMAP configuration\" }})))\n",
                            output_key
                        ));
                    }
                }
            }

            code.push_str("    });\n\n");
        }
        ActionNodeConfig::Code(config) => {
            code.push_str(&format!(
                "    // Action Node: {} (Code - {:?})\n",
                config.standard.name, config.language
            ));
            code.push_str(&format!("    let {}_node = adk_graph::node::FunctionNode::new(\"{}\", |ctx| async move {{\n", node_id, node_id));

            let output_key = if config.standard.mapping.output_key.is_empty() {
                "codeResult"
            } else {
                &config.standard.mapping.output_key
            };

            if config.code.is_empty() {
                code.push_str(&format!(
                    "        Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"error\": true, \"message\": \"No code provided\" }})))\n",
                    output_key
                ));
            } else {
                // Inject state variables as input to the JS context
                code.push_str("        // Collect state into a JSON object for the JS context\n");
                code.push_str("        let mut input_obj = serde_json::Map::new();\n");
                code.push_str("        for (k, v) in ctx.state.iter() {\n");
                code.push_str("            input_obj.insert(k.clone(), v.clone());\n");
                code.push_str("        }\n");
                code.push_str("        let input_json = serde_json::Value::Object(input_obj);\n\n");

                // Escape the user's JS code for embedding in a Rust raw string
                // Use a raw string with enough # delimiters to avoid conflicts
                let escaped_code = config.code.replace('\\', "\\\\");
                let escaped_code = escaped_code.replace('"', "\\\"");
                let escaped_code = escaped_code.replace('\n', "\\n");
                let escaped_code = escaped_code.replace('\r', "");
                let escaped_code = escaped_code.replace('\t', "\\t");

                // Create the JS execution using boa_engine
                code.push_str("        // Execute JavaScript code using boa_engine\n");
                code.push_str("        let js_result = std::thread::spawn(move || -> Result<serde_json::Value, String> {\n");
                code.push_str("            use boa_engine::{Context, Source, JsValue, JsError};\n");
                code.push_str("            use std::time::Instant;\n\n");

                // Time limit
                let time_limit = config.sandbox.time_limit;
                code.push_str(&format!(
                    "            let time_limit = std::time::Duration::from_millis({});\n",
                    time_limit
                ));
                code.push_str("            let start = Instant::now();\n\n");

                code.push_str("            let mut context = Context::default();\n\n");

                // Inject input state as a global `input` variable
                code.push_str("            // Inject state as global 'input' variable\n");
                code.push_str("            let input_str = serde_json::to_string(&input_json).unwrap_or_else(|_| \"{}\".to_string());\n");
                code.push_str(
                    "            let setup_code = format!(\"var input = {};\", input_str);\n",
                );
                code.push_str("            context.eval(Source::from_bytes(&setup_code))\n");
                code.push_str("                .map_err(|e| format!(\"Failed to inject input: {:?}\", e))?;\n\n");

                // Execute the user's code wrapped in a function that returns a value
                code.push_str(&format!("            let user_code = \"{}\";\n", escaped_code));
                // Wrap in an IIFE so the last expression is returned
                code.push_str(
                    "            let wrapped = format!(\"(function() {{ {} }})()\", user_code);\n",
                );
                code.push_str(
                    "            let result = context.eval(Source::from_bytes(&wrapped));\n\n",
                );

                // Check time limit
                code.push_str("            if start.elapsed() > time_limit {\n");
                code.push_str("                return Err(format!(\"Code execution exceeded time limit of {}ms\", time_limit.as_millis()));\n");
                code.push_str("            }\n\n");

                // Convert JsValue to serde_json::Value
                code.push_str("            match result {\n");
                code.push_str("                Ok(val) => {\n");
                code.push_str("                    // Convert JsValue to JSON\n");
                code.push_str(
                    "                    let json_val = js_value_to_json(&val, &mut context);\n",
                );
                code.push_str("                    Ok(json_val)\n");
                code.push_str("                }\n");
                code.push_str("                Err(e) => {\n");
                code.push_str("                    Err(format!(\"JavaScript error: {:?}\", e))\n");
                code.push_str("                }\n");
                code.push_str("            }\n");
                code.push_str("        }).join().unwrap_or_else(|_| Err(\"Code execution panicked\".to_string()));\n\n");

                // Handle result
                code.push_str("        match js_result {\n");
                code.push_str("            Ok(value) => {\n");
                code.push_str(&format!(
                    "                Ok(NodeOutput::new().with_update(\"{}\", value))\n",
                    output_key
                ));
                code.push_str("            }\n");
                code.push_str("            Err(err) => {\n");
                code.push_str(&format!(
                    "                Ok(NodeOutput::new().with_update(\"{}\", json!({{ \"error\": true, \"message\": err }})))\n",
                    output_key
                ));
                code.push_str("            }\n");
                code.push_str("        }\n");
            }

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
    let adk_version = project.settings.adk_version.as_deref().unwrap_or("0.3.2");
    let rust_edition = project.settings.rust_edition.as_deref().unwrap_or("2024");

    // Check if any function tool code uses specific crates
    let code_uses = |pattern: &str| -> bool {
        project.tool_configs.values().any(|tc| {
            if let ToolConfig::Function(fc) = tc { fc.code.contains(pattern) } else { false }
        })
    };

    let needs_reqwest = code_uses("reqwest::") || {
        use crate::codegen::action_nodes::ActionNodeConfig;
        project.action_nodes.values().any(|n| matches!(n, ActionNodeConfig::Http(_)))
    };
    let needs_lettre = code_uses("lettre::");
    let needs_base64 = code_uses("base64::");
    let needs_imap = code_uses("imap::");
    let needs_native_tls = code_uses("native_tls::");
    let needs_boa = {
        use crate::codegen::action_nodes::ActionNodeConfig;
        project.action_nodes.values().any(|n| matches!(n, ActionNodeConfig::Code(_)))
    };

    // Database dependencies based on action node db_type
    let (needs_sqlx_pg, needs_sqlx_mysql, needs_sqlx_sqlite, needs_mongodb, needs_redis) = {
        use crate::codegen::action_node_types::DatabaseType;
        use crate::codegen::action_nodes::ActionNodeConfig;
        let mut pg = false;
        let mut mysql = false;
        let mut sqlite = false;
        let mut mongo = false;
        let mut redis = false;
        for node in project.action_nodes.values() {
            if let ActionNodeConfig::Database(cfg) = node {
                match cfg.db_type {
                    DatabaseType::Postgresql => pg = true,
                    DatabaseType::Mysql => mysql = true,
                    DatabaseType::Sqlite => sqlite = true,
                    DatabaseType::Mongodb => mongo = true,
                    DatabaseType::Redis => redis = true,
                }
            }
        }
        (pg, mysql, sqlite, mongo, redis)
    };

    // Use path dependencies when running from a development workspace.
    // The workspace root is detected at compile time via CARGO_MANIFEST_DIR (adk-studio's parent).
    // At runtime, ADK_WORKSPACE_ROOT env var can override it.
    // Falls back to crates.io version deps if neither is available.
    let compile_time_root: Option<&str> = option_env!("CARGO_MANIFEST_DIR");
    let workspace_root: Option<String> = std::env::var("ADK_WORKSPACE_ROOT").ok().or_else(|| {
        compile_time_root.and_then(|manifest_dir| {
            std::path::Path::new(manifest_dir)
                .parent() // adk-studio -> workspace root
                .filter(|root| root.join("Cargo.toml").exists())
                .map(|p| p.to_string_lossy().to_string())
        })
    });
    let use_path_deps = workspace_root.is_some();
    let adk_root = workspace_root.unwrap_or_default();

    // Determine which adk-model features are needed based on providers used
    let providers = collect_providers(project);
    let mut model_features: Vec<&str> = Vec::new();
    if providers.contains("gemini") {
        model_features.push("gemini");
    }
    if providers.contains("openai") {
        model_features.push("openai");
    }
    if providers.contains("anthropic") {
        model_features.push("anthropic");
    }
    if providers.contains("deepseek") {
        model_features.push("deepseek");
    }
    if providers.contains("groq") {
        model_features.push("groq");
    }
    if providers.contains("ollama") {
        model_features.push("ollama");
    }
    if providers.contains("fireworks") {
        model_features.push("fireworks");
    }
    if providers.contains("together") {
        model_features.push("together");
    }
    if providers.contains("mistral") {
        model_features.push("mistral");
    }
    if providers.contains("perplexity") {
        model_features.push("perplexity");
    }
    if providers.contains("cerebras") {
        model_features.push("cerebras");
    }
    if providers.contains("sambanova") {
        model_features.push("sambanova");
    }
    if providers.contains("bedrock") {
        model_features.push("bedrock");
    }
    if providers.contains("azure-ai") {
        model_features.push("azure-ai");
    }
    // Default to gemini if no providers detected
    if model_features.is_empty() {
        model_features.push("gemini");
    }
    let features_str =
        model_features.iter().map(|f| format!("\"{}\"", f)).collect::<Vec<_>>().join(", ");

    let adk_deps = if use_path_deps {
        format!(
            r#"adk-agent = {{ path = "{}/adk-agent" }}
adk-core = {{ path = "{}/adk-core" }}
adk-model = {{ path = "{}/adk-model", default-features = false, features = [{}] }}
adk-tool = {{ path = "{}/adk-tool" }}
adk-graph = {{ path = "{}/adk-graph" }}"#,
            adk_root, adk_root, adk_root, features_str, adk_root, adk_root
        )
    } else {
        format!(
            r#"adk-agent = "{}"
adk-core = "{}"
adk-model = {{ version = "{}", default-features = false, features = [{}] }}
adk-tool = "{}"
adk-graph = "{}""#,
            adk_version, adk_version, adk_version, features_str, adk_version, adk_version
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
    if needs_imap {
        deps.push_str("imap = \"3\"\n");
    }
    if needs_native_tls {
        deps.push_str("native-tls = \"0.2\"\n");
    }
    if needs_boa {
        deps.push_str("boa_engine = { version = \"0.20\", features = [\"annex-b\"] }\n");
    }

    // Database dependencies
    let needs_sqlx = needs_sqlx_pg || needs_sqlx_mysql || needs_sqlx_sqlite;
    if needs_sqlx {
        let mut sqlx_features = vec!["runtime-tokio"];
        if needs_sqlx_pg {
            sqlx_features.push("postgres");
        }
        if needs_sqlx_mysql {
            sqlx_features.push("mysql");
        }
        if needs_sqlx_sqlite {
            sqlx_features.push("sqlite");
        }
        let features_str =
            sqlx_features.iter().map(|f| format!("\"{}\"", f)).collect::<Vec<_>>().join(", ");
        deps.push_str(&format!("sqlx = {{ version = \"0.8\", features = [{}] }}\n", features_str));
    }
    if needs_mongodb {
        deps.push_str("mongodb = \"3\"\nfutures = \"0.3\"\n");
    }
    if needs_redis {
        deps.push_str("redis = { version = \"0.27\", features = [\"tokio-comp\"] }\n");
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
