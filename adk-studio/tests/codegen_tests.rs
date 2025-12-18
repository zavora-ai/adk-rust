//! Integration tests for adk-studio code generation

use adk_studio::codegen::generate_rust_project;
use adk_studio::schema::{AgentSchema, AgentType, ProjectSchema, Route, ToolConfig, FunctionToolConfig, FunctionParameter, ParamType, McpToolConfig};
use std::collections::HashMap;

fn project(name: &str, agents: HashMap<String, AgentSchema>) -> ProjectSchema {
    let mut p = ProjectSchema::new(name);
    p.agents = agents;
    p
}

fn project_with_tools(name: &str, agents: HashMap<String, AgentSchema>, tool_configs: HashMap<String, ToolConfig>) -> ProjectSchema {
    let mut p = ProjectSchema::new(name);
    p.agents = agents;
    p.tool_configs = tool_configs;
    p
}

fn llm_agent(instruction: &str) -> AgentSchema {
    AgentSchema {
        agent_type: AgentType::Llm,
        model: Some("gemini-2.0-flash".to_string()),
        instruction: instruction.to_string(),
        tools: vec![],
        sub_agents: vec![],
        position: Default::default(),
        max_iterations: None,
        routes: vec![],
    }
}

fn get_main_rs(project: &ProjectSchema) -> String {
    let gen = generate_rust_project(project).unwrap();
    gen.files.iter().find(|f| f.path == "src/main.rs").unwrap().content.clone()
}

fn get_cargo_toml(project: &ProjectSchema) -> String {
    let gen = generate_rust_project(project).unwrap();
    gen.files.iter().find(|f| f.path == "Cargo.toml").unwrap().content.clone()
}

// =============================================================================
// LLM Agent
// =============================================================================

#[test]
fn llm_agent_generates_builder() {
    let mut agents = HashMap::new();
    agents.insert("assistant".to_string(), llm_agent("You are helpful."));
    
    let code = get_main_rs(&project("test", agents));
    assert!(code.contains("LlmAgentBuilder::new"));
}

#[test]
fn llm_agent_includes_model() {
    let mut agents = HashMap::new();
    agents.insert("assistant".to_string(), llm_agent("Test."));
    
    let code = get_main_rs(&project("test", agents));
    assert!(code.contains("gemini-2.0-flash"));
}

#[test]
fn llm_agent_includes_instruction() {
    let mut agents = HashMap::new();
    agents.insert("assistant".to_string(), llm_agent("Be concise and direct."));
    
    let code = get_main_rs(&project("test", agents));
    assert!(code.contains("Be concise and direct"));
}

#[test]
fn llm_agent_with_google_search() {
    let mut agents = HashMap::new();
    let mut agent = llm_agent("Search the web.");
    agent.tools = vec!["google_search".to_string()];
    agents.insert("searcher".to_string(), agent);
    
    let code = get_main_rs(&project("test", agents));
    assert!(code.contains("GoogleSearchTool::new()"));
}

#[test]
fn llm_agent_with_exit_loop() {
    let mut agents = HashMap::new();
    let mut agent = llm_agent("Refine content.");
    agent.tools = vec!["exit_loop".to_string()];
    agents.insert("refiner".to_string(), agent);
    
    let code = get_main_rs(&project("test", agents));
    assert!(code.contains("ExitLoopTool::new()"));
}

// =============================================================================
// Sequential Agent
// =============================================================================

#[test]
fn sequential_agent_generates_container() {
    let mut agents = HashMap::new();
    agents.insert("step1".to_string(), llm_agent("First step."));
    agents.insert("step2".to_string(), llm_agent("Second step."));
    agents.insert("pipeline".to_string(), AgentSchema {
        agent_type: AgentType::Sequential,
        model: None,
        instruction: String::new(),
        tools: vec![],
        sub_agents: vec!["step1".to_string(), "step2".to_string()],
        position: Default::default(),
        max_iterations: None,
        routes: vec![],
    });
    
    let code = get_main_rs(&project("test", agents));
    assert!(code.contains("SequentialAgent::new"));
}

#[test]
fn sequential_agent_includes_sub_agents() {
    let mut agents = HashMap::new();
    agents.insert("writer".to_string(), llm_agent("Write."));
    agents.insert("editor".to_string(), llm_agent("Edit."));
    agents.insert("pipeline".to_string(), AgentSchema {
        agent_type: AgentType::Sequential,
        model: None,
        instruction: String::new(),
        tools: vec![],
        sub_agents: vec!["writer".to_string(), "editor".to_string()],
        position: Default::default(),
        max_iterations: None,
        routes: vec![],
    });
    
    let code = get_main_rs(&project("test", agents));
    assert!(code.contains("writer_agent"));
    assert!(code.contains("editor_agent"));
}

#[test]
fn sequential_sub_agent_with_tools() {
    let mut agents = HashMap::new();
    let mut researcher = llm_agent("Research.");
    researcher.tools = vec!["google_search".to_string()];
    agents.insert("researcher".to_string(), researcher);
    agents.insert("summarizer".to_string(), llm_agent("Summarize."));
    agents.insert("pipeline".to_string(), AgentSchema {
        agent_type: AgentType::Sequential,
        model: None,
        instruction: String::new(),
        tools: vec![],
        sub_agents: vec!["researcher".to_string(), "summarizer".to_string()],
        position: Default::default(),
        max_iterations: None,
        routes: vec![],
    });
    
    let code = get_main_rs(&project("test", agents));
    assert!(code.contains("researcher_builder"));
    assert!(code.contains("GoogleSearchTool"));
}

// =============================================================================
// Loop Agent
// =============================================================================

#[test]
fn loop_agent_generates_container() {
    let mut agents = HashMap::new();
    let mut worker = llm_agent("Work.");
    worker.tools = vec!["exit_loop".to_string()];
    agents.insert("worker".to_string(), worker);
    agents.insert("looper".to_string(), AgentSchema {
        agent_type: AgentType::Loop,
        model: None,
        instruction: String::new(),
        tools: vec![],
        sub_agents: vec!["worker".to_string()],
        position: Default::default(),
        max_iterations: Some(5),
        routes: vec![],
    });
    
    let code = get_main_rs(&project("test", agents));
    assert!(code.contains("LoopAgent::new"));
}

#[test]
fn loop_agent_with_max_iterations() {
    let mut agents = HashMap::new();
    agents.insert("worker".to_string(), llm_agent("Work."));
    agents.insert("looper".to_string(), AgentSchema {
        agent_type: AgentType::Loop,
        model: None,
        instruction: String::new(),
        tools: vec![],
        sub_agents: vec!["worker".to_string()],
        position: Default::default(),
        max_iterations: Some(7),
        routes: vec![],
    });
    
    let code = get_main_rs(&project("test", agents));
    assert!(code.contains("with_max_iterations(7)"));
}

#[test]
fn loop_agent_default_iterations() {
    let mut agents = HashMap::new();
    agents.insert("worker".to_string(), llm_agent("Work."));
    agents.insert("looper".to_string(), AgentSchema {
        agent_type: AgentType::Loop,
        model: None,
        instruction: String::new(),
        tools: vec![],
        sub_agents: vec!["worker".to_string()],
        position: Default::default(),
        max_iterations: None,
        routes: vec![],
    });
    
    let code = get_main_rs(&project("test", agents));
    assert!(code.contains("with_max_iterations(3)"));
}

#[test]
fn loop_agent_filters_exit_loop_from_output() {
    let mut agents = HashMap::new();
    let mut worker = llm_agent("Work.");
    worker.tools = vec!["exit_loop".to_string()];
    agents.insert("worker".to_string(), worker);
    agents.insert("looper".to_string(), AgentSchema {
        agent_type: AgentType::Loop,
        model: None,
        instruction: String::new(),
        tools: vec![],
        sub_agents: vec!["worker".to_string()],
        position: Default::default(),
        max_iterations: Some(3),
        routes: vec![],
    });
    
    let code = get_main_rs(&project("test", agents));
    assert!(code.contains(r#"replace("exit_loop", "")"#));
}

// =============================================================================
// Parallel Agent
// =============================================================================

#[test]
fn parallel_agent_generates_container() {
    let mut agents = HashMap::new();
    agents.insert("analyzer1".to_string(), llm_agent("Analyze sentiment."));
    agents.insert("analyzer2".to_string(), llm_agent("Extract entities."));
    agents.insert("parallel".to_string(), AgentSchema {
        agent_type: AgentType::Parallel,
        model: None,
        instruction: String::new(),
        tools: vec![],
        sub_agents: vec!["analyzer1".to_string(), "analyzer2".to_string()],
        position: Default::default(),
        max_iterations: None,
        routes: vec![],
    });
    
    let code = get_main_rs(&project("test", agents));
    assert!(code.contains("ParallelAgent::new"));
}

#[test]
fn parallel_agent_includes_sub_agents() {
    let mut agents = HashMap::new();
    agents.insert("task_a".to_string(), llm_agent("Task A."));
    agents.insert("task_b".to_string(), llm_agent("Task B."));
    agents.insert("parallel".to_string(), AgentSchema {
        agent_type: AgentType::Parallel,
        model: None,
        instruction: String::new(),
        tools: vec![],
        sub_agents: vec!["task_a".to_string(), "task_b".to_string()],
        position: Default::default(),
        max_iterations: None,
        routes: vec![],
    });
    
    let code = get_main_rs(&project("test", agents));
    assert!(code.contains("task_a_agent"));
    assert!(code.contains("task_b_agent"));
}

// =============================================================================
// Router Agent
// =============================================================================

#[test]
fn router_agent_generates_classifier() {
    let mut agents = HashMap::new();
    agents.insert("router".to_string(), AgentSchema {
        agent_type: AgentType::Router,
        model: Some("gemini-2.0-flash".to_string()),
        instruction: "Classify the request.".to_string(),
        tools: vec![],
        sub_agents: vec![],
        position: Default::default(),
        max_iterations: None,
        routes: vec![
            Route { condition: "technical".to_string(), target: "tech".to_string() },
            Route { condition: "general".to_string(), target: "general".to_string() },
        ],
    });
    agents.insert("tech".to_string(), llm_agent("Handle tech."));
    agents.insert("general".to_string(), llm_agent("Handle general."));
    
    let code = get_main_rs(&project("test", agents));
    assert!(code.contains("classification"));
}

#[test]
fn router_agent_includes_routes() {
    let mut agents = HashMap::new();
    agents.insert("router".to_string(), AgentSchema {
        agent_type: AgentType::Router,
        model: Some("gemini-2.0-flash".to_string()),
        instruction: "Route.".to_string(),
        tools: vec![],
        sub_agents: vec![],
        position: Default::default(),
        max_iterations: None,
        routes: vec![
            Route { condition: "billing".to_string(), target: "billing_agent".to_string() },
            Route { condition: "support".to_string(), target: "support_agent".to_string() },
        ],
    });
    agents.insert("billing_agent".to_string(), llm_agent("Billing."));
    agents.insert("support_agent".to_string(), llm_agent("Support."));
    
    let code = get_main_rs(&project("test", agents));
    assert!(code.contains("billing"));
    assert!(code.contains("support"));
}

// =============================================================================
// Function Tool
// =============================================================================

#[test]
fn function_tool_generates_code() {
    let mut agents = HashMap::new();
    let mut agent = llm_agent("Use calculator.");
    agent.tools = vec!["function_add".to_string()];
    agents.insert("calc".to_string(), agent);
    
    let mut tool_configs = HashMap::new();
    tool_configs.insert("calc_function_add".to_string(), ToolConfig::Function(FunctionToolConfig {
        name: "add".to_string(),
        description: "Add two numbers".to_string(),
        parameters: vec![
            FunctionParameter { name: "a".to_string(), param_type: ParamType::Number, description: "First".to_string(), required: true },
            FunctionParameter { name: "b".to_string(), param_type: ParamType::Number, description: "Second".to_string(), required: true },
        ],
        code: "Ok(json!(1 + 1))".to_string(),
    }));
    
    let code = get_main_rs(&project_with_tools("test", agents, tool_configs));
    assert!(code.contains("async fn add_fn"));
    assert!(code.contains("FunctionTool::new"));
}

#[test]
fn function_tool_includes_description() {
    let mut agents = HashMap::new();
    let mut agent = llm_agent("Use tool.");
    agent.tools = vec!["function_greet".to_string()];
    agents.insert("greeter".to_string(), agent);
    
    let mut tool_configs = HashMap::new();
    tool_configs.insert("greeter_function_greet".to_string(), ToolConfig::Function(FunctionToolConfig {
        name: "greet".to_string(),
        description: "Greet a person by name".to_string(),
        parameters: vec![],
        code: "Ok(json!(\"Hello\"))".to_string(),
    }));
    
    let code = get_main_rs(&project_with_tools("test", agents, tool_configs));
    assert!(code.contains("Greet a person by name"));
}

// =============================================================================
// MCP Tool
// =============================================================================

#[test]
fn mcp_tool_generates_toolset() {
    let mut agents = HashMap::new();
    let mut agent = llm_agent("Use MCP.");
    agent.tools = vec!["mcp".to_string()];
    agents.insert("mcp_user".to_string(), agent);
    
    let mut tool_configs = HashMap::new();
    tool_configs.insert("mcp_user_mcp".to_string(), ToolConfig::Mcp(McpToolConfig {
        server_command: "npx".to_string(),
        server_args: vec!["-y".to_string(), "@modelcontextprotocol/server-filesystem".to_string()],
        tool_filter: vec![],
    }));
    
    let code = get_main_rs(&project_with_tools("test", agents, tool_configs));
    assert!(code.contains("McpToolset"));
    assert!(code.contains("TokioChildProcess"));
}

#[test]
fn mcp_tool_includes_command() {
    let mut agents = HashMap::new();
    let mut agent = llm_agent("Use MCP.");
    agent.tools = vec!["mcp".to_string()];
    agents.insert("agent".to_string(), agent);
    
    let mut tool_configs = HashMap::new();
    tool_configs.insert("agent_mcp".to_string(), ToolConfig::Mcp(McpToolConfig {
        server_command: "uvx".to_string(),
        server_args: vec!["mcp-server-git".to_string()],
        tool_filter: vec![],
    }));
    
    let code = get_main_rs(&project_with_tools("test", agents, tool_configs));
    assert!(code.contains("uvx"));
    assert!(code.contains("mcp-server-git"));
}

// =============================================================================
// Browser Tool
// =============================================================================

#[test]
fn browser_tool_generates_session() {
    let mut agents = HashMap::new();
    let mut agent = llm_agent("Browse web.");
    agent.tools = vec!["browser".to_string()];
    agents.insert("browser_agent".to_string(), agent);
    
    let code = get_main_rs(&project("test", agents));
    assert!(code.contains("BrowserSession"));
    assert!(code.contains("BrowserToolset"));
}

#[test]
fn browser_tool_adds_dependency() {
    let mut agents = HashMap::new();
    let mut agent = llm_agent("Browse.");
    agent.tools = vec!["browser".to_string()];
    agents.insert("agent".to_string(), agent);
    
    let toml = get_cargo_toml(&project("test", agents));
    assert!(toml.contains("adk-browser"));
}

// =============================================================================
// Cargo.toml
// =============================================================================

#[test]
fn cargo_toml_has_package_name() {
    let mut agents = HashMap::new();
    agents.insert("agent".to_string(), llm_agent("Test."));
    
    let toml = get_cargo_toml(&project("my_project", agents));
    assert!(toml.contains("name = \"my_project\""));
}

#[test]
fn cargo_toml_has_core_dependencies() {
    let mut agents = HashMap::new();
    agents.insert("agent".to_string(), llm_agent("Test."));
    
    let toml = get_cargo_toml(&project("test", agents));
    assert!(toml.contains("adk-graph"));
    assert!(toml.contains("adk-agent"));
    assert!(toml.contains("adk-model"));
    assert!(toml.contains("adk-tool"));
    assert!(toml.contains("adk-core"));
}

#[test]
fn cargo_toml_has_mcp_deps_when_needed() {
    let mut agents = HashMap::new();
    let mut agent = llm_agent("Use MCP.");
    agent.tools = vec!["mcp".to_string()];
    agents.insert("agent".to_string(), agent);
    
    let mut tool_configs = HashMap::new();
    tool_configs.insert("agent_mcp".to_string(), ToolConfig::Mcp(McpToolConfig {
        server_command: "npx".to_string(),
        server_args: vec![],
        tool_filter: vec![],
    }));
    
    let toml = get_cargo_toml(&project_with_tools("test", agents, tool_configs));
    assert!(toml.contains("rmcp"));
}

// =============================================================================
// Code Quality
// =============================================================================

#[test]
fn generated_code_allows_unused() {
    let mut agents = HashMap::new();
    agents.insert("agent".to_string(), llm_agent("Test."));
    
    let code = get_main_rs(&project("test", agents));
    assert!(code.contains("#![allow(unused_imports, unused_variables)]"));
}
