//! Integration tests for adk-studio code generation

use adk_studio::codegen::generate_rust_project;
use adk_studio::schema::{
    AgentSchema, AgentType, Edge, FunctionParameter, FunctionToolConfig, McpToolConfig, ParamType,
    ProjectSchema, Route, ToolConfig,
};
use std::collections::HashMap;

fn project(name: &str, agents: HashMap<String, AgentSchema>) -> ProjectSchema {
    let mut p = ProjectSchema::new(name);
    p.agents = agents.clone();
    // Add default workflow edges for single-agent projects
    if let Some(agent_id) = agents.keys().next() {
        p.workflow.edges = vec![Edge::new("START", agent_id), Edge::new(agent_id, "END")];
    }
    p
}

fn project_with_tools(
    name: &str,
    agents: HashMap<String, AgentSchema>,
    tool_configs: HashMap<String, ToolConfig>,
) -> ProjectSchema {
    let mut p = ProjectSchema::new(name);
    p.agents = agents.clone();
    p.tool_configs = tool_configs;
    // Add default workflow edges for single-agent projects
    if let Some(agent_id) = agents.keys().next() {
        p.workflow.edges = vec![Edge::new("START", agent_id), Edge::new(agent_id, "END")];
    }
    p
}

/// Create a project with explicit workflow edges
fn project_with_workflow(
    name: &str,
    agents: HashMap<String, AgentSchema>,
    edges: Vec<Edge>,
) -> ProjectSchema {
    let mut p = ProjectSchema::new(name);
    p.agents = agents;
    p.workflow.edges = edges;
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
    let r#gen = generate_rust_project(project).unwrap();
    r#gen.files.iter().find(|f| f.path == "src/main.rs").unwrap().content.clone()
}

fn get_cargo_toml(project: &ProjectSchema) -> String {
    let r#gen = generate_rust_project(project).unwrap();
    r#gen.files.iter().find(|f| f.path == "Cargo.toml").unwrap().content.clone()
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
    agents.insert(
        "pipeline".to_string(),
        AgentSchema {
            agent_type: AgentType::Sequential,
            model: None,
            instruction: String::new(),
            tools: vec![],
            sub_agents: vec!["step1".to_string(), "step2".to_string()],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );

    let edges = vec![Edge::new("START", "pipeline"), Edge::new("pipeline", "END")];
    let code = get_main_rs(&project_with_workflow("test", agents, edges));
    assert!(code.contains("SequentialAgent::new"));
}

#[test]
fn sequential_agent_includes_sub_agents() {
    let mut agents = HashMap::new();
    agents.insert("writer".to_string(), llm_agent("Write."));
    agents.insert("editor".to_string(), llm_agent("Edit."));
    agents.insert(
        "pipeline".to_string(),
        AgentSchema {
            agent_type: AgentType::Sequential,
            model: None,
            instruction: String::new(),
            tools: vec![],
            sub_agents: vec!["writer".to_string(), "editor".to_string()],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );

    let edges = vec![Edge::new("START", "pipeline"), Edge::new("pipeline", "END")];
    let code = get_main_rs(&project_with_workflow("test", agents, edges));
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
    agents.insert(
        "pipeline".to_string(),
        AgentSchema {
            agent_type: AgentType::Sequential,
            model: None,
            instruction: String::new(),
            tools: vec![],
            sub_agents: vec!["researcher".to_string(), "summarizer".to_string()],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );

    let edges = vec![Edge::new("START", "pipeline"), Edge::new("pipeline", "END")];
    let code = get_main_rs(&project_with_workflow("test", agents, edges));
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
    agents.insert(
        "looper".to_string(),
        AgentSchema {
            agent_type: AgentType::Loop,
            model: None,
            instruction: String::new(),
            tools: vec![],
            sub_agents: vec!["worker".to_string()],
            position: Default::default(),
            max_iterations: Some(5),
            routes: vec![],
        },
    );

    let edges = vec![Edge::new("START", "looper"), Edge::new("looper", "END")];
    let code = get_main_rs(&project_with_workflow("test", agents, edges));
    assert!(code.contains("LoopAgent::new"));
}

#[test]
fn loop_agent_with_max_iterations() {
    let mut agents = HashMap::new();
    agents.insert("worker".to_string(), llm_agent("Work."));
    agents.insert(
        "looper".to_string(),
        AgentSchema {
            agent_type: AgentType::Loop,
            model: None,
            instruction: String::new(),
            tools: vec![],
            sub_agents: vec!["worker".to_string()],
            position: Default::default(),
            max_iterations: Some(7),
            routes: vec![],
        },
    );

    let edges = vec![Edge::new("START", "looper"), Edge::new("looper", "END")];
    let code = get_main_rs(&project_with_workflow("test", agents, edges));
    assert!(code.contains("with_max_iterations(7)"));
}

#[test]
fn loop_agent_default_iterations() {
    let mut agents = HashMap::new();
    agents.insert("worker".to_string(), llm_agent("Work."));
    agents.insert(
        "looper".to_string(),
        AgentSchema {
            agent_type: AgentType::Loop,
            model: None,
            instruction: String::new(),
            tools: vec![],
            sub_agents: vec!["worker".to_string()],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );

    let edges = vec![Edge::new("START", "looper"), Edge::new("looper", "END")];
    let code = get_main_rs(&project_with_workflow("test", agents, edges));
    assert!(code.contains("with_max_iterations(3)"));
}

#[test]
fn loop_agent_filters_exit_loop_from_output() {
    let mut agents = HashMap::new();
    let mut worker = llm_agent("Work.");
    worker.tools = vec!["exit_loop".to_string()];
    agents.insert("worker".to_string(), worker);
    agents.insert(
        "looper".to_string(),
        AgentSchema {
            agent_type: AgentType::Loop,
            model: None,
            instruction: String::new(),
            tools: vec![],
            sub_agents: vec!["worker".to_string()],
            position: Default::default(),
            max_iterations: Some(3),
            routes: vec![],
        },
    );

    let edges = vec![Edge::new("START", "looper"), Edge::new("looper", "END")];
    let code = get_main_rs(&project_with_workflow("test", agents, edges));
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
    agents.insert(
        "parallel".to_string(),
        AgentSchema {
            agent_type: AgentType::Parallel,
            model: None,
            instruction: String::new(),
            tools: vec![],
            sub_agents: vec!["analyzer1".to_string(), "analyzer2".to_string()],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );

    let edges = vec![Edge::new("START", "parallel"), Edge::new("parallel", "END")];
    let code = get_main_rs(&project_with_workflow("test", agents, edges));
    assert!(code.contains("ParallelAgent::new"));
}

#[test]
fn parallel_agent_includes_sub_agents() {
    let mut agents = HashMap::new();
    agents.insert("task_a".to_string(), llm_agent("Task A."));
    agents.insert("task_b".to_string(), llm_agent("Task B."));
    agents.insert(
        "parallel".to_string(),
        AgentSchema {
            agent_type: AgentType::Parallel,
            model: None,
            instruction: String::new(),
            tools: vec![],
            sub_agents: vec!["task_a".to_string(), "task_b".to_string()],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );

    let edges = vec![Edge::new("START", "parallel"), Edge::new("parallel", "END")];
    let code = get_main_rs(&project_with_workflow("test", agents, edges));
    assert!(code.contains("task_a_agent"));
    assert!(code.contains("task_b_agent"));
}

// =============================================================================
// Router Agent
// =============================================================================

#[test]
fn router_agent_generates_classifier() {
    let mut agents = HashMap::new();
    agents.insert(
        "router".to_string(),
        AgentSchema {
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
        },
    );
    agents.insert("tech".to_string(), llm_agent("Handle tech."));
    agents.insert("general".to_string(), llm_agent("Handle general."));

    let edges = vec![
        Edge::new("START", "router"),
        Edge::new("router", "tech"),
        Edge::new("router", "general"),
        Edge::new("tech", "END"),
        Edge::new("general", "END"),
    ];
    let code = get_main_rs(&project_with_workflow("test", agents, edges));
    assert!(code.contains("classification"));
}

#[test]
fn router_agent_includes_routes() {
    let mut agents = HashMap::new();
    agents.insert(
        "router".to_string(),
        AgentSchema {
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
        },
    );
    agents.insert("billing_agent".to_string(), llm_agent("Billing."));
    agents.insert("support_agent".to_string(), llm_agent("Support."));

    let edges = vec![
        Edge::new("START", "router"),
        Edge::new("router", "billing_agent"),
        Edge::new("router", "support_agent"),
        Edge::new("billing_agent", "END"),
        Edge::new("support_agent", "END"),
    ];
    let code = get_main_rs(&project_with_workflow("test", agents, edges));
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
    tool_configs.insert(
        "calc_function_add".to_string(),
        ToolConfig::Function(FunctionToolConfig {
            name: "add".to_string(),
            description: "Add two numbers".to_string(),
            parameters: vec![
                FunctionParameter {
                    name: "a".to_string(),
                    param_type: ParamType::Number,
                    description: "First".to_string(),
                    required: true,
                },
                FunctionParameter {
                    name: "b".to_string(),
                    param_type: ParamType::Number,
                    description: "Second".to_string(),
                    required: true,
                },
            ],
            code: "Ok(json!(1 + 1))".to_string(),
        }),
    );

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
    tool_configs.insert(
        "greeter_function_greet".to_string(),
        ToolConfig::Function(FunctionToolConfig {
            name: "greet".to_string(),
            description: "Greet a person by name".to_string(),
            parameters: vec![],
            code: "Ok(json!(\"Hello\"))".to_string(),
        }),
    );

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
    tool_configs.insert(
        "mcp_user_mcp".to_string(),
        ToolConfig::Mcp(McpToolConfig {
            server_command: "npx".to_string(),
            server_args: vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-filesystem".to_string(),
            ],
            tool_filter: vec![],
        }),
    );

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
    tool_configs.insert(
        "agent_mcp".to_string(),
        ToolConfig::Mcp(McpToolConfig {
            server_command: "uvx".to_string(),
            server_args: vec!["mcp-server-git".to_string()],
            tool_filter: vec![],
        }),
    );

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
    tool_configs.insert(
        "agent_mcp".to_string(),
        ToolConfig::Mcp(McpToolConfig {
            server_command: "npx".to_string(),
            server_args: vec![],
            tool_filter: vec![],
        }),
    );

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

// =============================================================================
// Code Comments (Requirement 12.2)
// =============================================================================

#[test]
fn generated_code_has_workflow_header_comment() {
    let mut agents = HashMap::new();
    agents.insert("assistant".to_string(), llm_agent("You are helpful."));

    let code = get_main_rs(&project("My Test Project", agents));
    assert!(code.contains("//! My Test Project"), "Should have project name in header");
    assert!(code.contains("//! ## Workflow Structure"), "Should have workflow structure section");
    assert!(code.contains("//! ## Agents"), "Should have agents section");
    assert!(code.contains("//! ## Execution Flow"), "Should have execution flow section");
}

#[test]
fn generated_code_documents_agent_tools() {
    let mut agents = HashMap::new();
    let mut agent = llm_agent("Search the web.");
    agent.tools = vec!["google_search".to_string()];
    agents.insert("searcher".to_string(), agent);

    let code = get_main_rs(&project("test", agents));
    assert!(code.contains("Tools: google_search"), "Should document agent tools");
}

#[test]
fn generated_code_documents_sequential_flow() {
    let mut agents = HashMap::new();
    agents.insert("writer".to_string(), llm_agent("Write."));
    agents.insert("editor".to_string(), llm_agent("Edit."));
    agents.insert(
        "pipeline".to_string(),
        AgentSchema {
            agent_type: AgentType::Sequential,
            model: None,
            instruction: String::new(),
            tools: vec![],
            sub_agents: vec!["writer".to_string(), "editor".to_string()],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );

    let edges = vec![Edge::new("START", "pipeline"), Edge::new("pipeline", "END")];
    let code = get_main_rs(&project_with_workflow("test", agents, edges));
    assert!(code.contains("Sub-agents: writer → editor"), "Should document sub-agent flow");
}

// =============================================================================
// Environment Variable Warnings (Requirement 12.10)
// =============================================================================

#[test]
fn generated_code_documents_env_vars() {
    let mut agents = HashMap::new();
    agents.insert("assistant".to_string(), llm_agent("You are helpful."));

    let code = get_main_rs(&project("test", agents));
    assert!(code.contains("GOOGLE_API_KEY"), "Should document required API key");
    assert!(code.contains("Required Environment Variables"), "Should have env vars section");
}

#[test]
fn generated_code_documents_browser_env_var() {
    let mut agents = HashMap::new();
    let mut agent = llm_agent("Browse the web.");
    agent.tools = vec!["browser".to_string()];
    agents.insert("browser_agent".to_string(), agent);

    let code = get_main_rs(&project("test", agents));
    assert!(code.contains("CHROME_PATH"), "Should document browser env var");
}

// =============================================================================
// Template Code Generation (Requirement 12.9)
// =============================================================================

/// Helper to create a template-like project for testing
fn create_template_project(
    name: &str,
    agents: HashMap<String, AgentSchema>,
    edges: Vec<Edge>,
) -> ProjectSchema {
    let mut p = ProjectSchema::new(name);
    p.agents = agents;
    p.workflow.edges = edges;
    p
}

#[test]
fn template_simple_chat_generates_code() {
    let mut agents = HashMap::new();
    agents.insert(
        "chat_agent".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.0-flash".to_string()),
            instruction: "You are a helpful assistant.".to_string(),
            tools: vec![],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );

    let edges = vec![Edge::new("START", "chat_agent"), Edge::new("chat_agent", "END")];

    let project = create_template_project("Simple Chat", agents, edges);
    let result = generate_rust_project(&project);
    assert!(result.is_ok(), "Simple chat template should generate valid code");

    let generated = result.unwrap();
    let main_rs = generated.files.iter().find(|f| f.path == "src/main.rs").unwrap();
    assert!(main_rs.content.contains("LlmAgentBuilder"));
    assert!(main_rs.content.contains("chat_agent"));
}

#[test]
fn template_research_pipeline_generates_code() {
    let mut agents = HashMap::new();
    agents.insert(
        "researcher".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.0-flash".to_string()),
            instruction: "Research the topic.".to_string(),
            tools: vec!["google_search".to_string()],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );
    agents.insert(
        "summarizer".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.0-flash".to_string()),
            instruction: "Summarize the research.".to_string(),
            tools: vec![],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );
    agents.insert(
        "research_pipeline".to_string(),
        AgentSchema {
            agent_type: AgentType::Sequential,
            model: None,
            instruction: String::new(),
            tools: vec![],
            sub_agents: vec!["researcher".to_string(), "summarizer".to_string()],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );

    let edges =
        vec![Edge::new("START", "research_pipeline"), Edge::new("research_pipeline", "END")];

    let project = create_template_project("Research Pipeline", agents, edges);
    let result = generate_rust_project(&project);
    assert!(result.is_ok(), "Research pipeline template should generate valid code");

    let generated = result.unwrap();
    let main_rs = generated.files.iter().find(|f| f.path == "src/main.rs").unwrap();
    assert!(main_rs.content.contains("SequentialAgent"));
    assert!(main_rs.content.contains("GoogleSearchTool"));
}

#[test]
fn template_content_refiner_generates_code() {
    let mut agents = HashMap::new();
    agents.insert(
        "improver".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.0-flash".to_string()),
            instruction: "Improve the content.".to_string(),
            tools: vec![],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );
    agents.insert(
        "reviewer".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.0-flash".to_string()),
            instruction: "Review and decide if done.".to_string(),
            tools: vec!["exit_loop".to_string()],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );
    agents.insert(
        "content_refiner".to_string(),
        AgentSchema {
            agent_type: AgentType::Loop,
            model: None,
            instruction: String::new(),
            tools: vec![],
            sub_agents: vec!["improver".to_string(), "reviewer".to_string()],
            position: Default::default(),
            max_iterations: Some(3),
            routes: vec![],
        },
    );

    let edges = vec![Edge::new("START", "content_refiner"), Edge::new("content_refiner", "END")];

    let project = create_template_project("Content Refiner", agents, edges);
    let result = generate_rust_project(&project);
    assert!(result.is_ok(), "Content refiner template should generate valid code");

    let generated = result.unwrap();
    let main_rs = generated.files.iter().find(|f| f.path == "src/main.rs").unwrap();
    assert!(main_rs.content.contains("LoopAgent"));
    assert!(main_rs.content.contains("ExitLoopTool"));
    assert!(main_rs.content.contains("with_max_iterations(3)"));
}

#[test]
fn template_parallel_analyzer_generates_code() {
    let mut agents = HashMap::new();
    agents.insert(
        "sentiment_analyzer".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.0-flash".to_string()),
            instruction: "Analyze sentiment.".to_string(),
            tools: vec![],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );
    agents.insert(
        "entity_extractor".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.0-flash".to_string()),
            instruction: "Extract entities.".to_string(),
            tools: vec![],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );
    agents.insert(
        "parallel_analyzer".to_string(),
        AgentSchema {
            agent_type: AgentType::Parallel,
            model: None,
            instruction: String::new(),
            tools: vec![],
            sub_agents: vec!["sentiment_analyzer".to_string(), "entity_extractor".to_string()],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );

    let edges =
        vec![Edge::new("START", "parallel_analyzer"), Edge::new("parallel_analyzer", "END")];

    let project = create_template_project("Parallel Analyzer", agents, edges);
    let result = generate_rust_project(&project);
    assert!(result.is_ok(), "Parallel analyzer template should generate valid code");

    let generated = result.unwrap();
    let main_rs = generated.files.iter().find(|f| f.path == "src/main.rs").unwrap();
    assert!(main_rs.content.contains("ParallelAgent"));
}

#[test]
fn template_support_router_generates_code() {
    let mut agents = HashMap::new();
    agents.insert(
        "router".to_string(),
        AgentSchema {
            agent_type: AgentType::Router,
            model: Some("gemini-2.0-flash".to_string()),
            instruction: "Classify the request.".to_string(),
            tools: vec![],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![
                Route { condition: "technical".to_string(), target: "tech_support".to_string() },
                Route { condition: "billing".to_string(), target: "billing_support".to_string() },
                Route { condition: "general".to_string(), target: "general_support".to_string() },
            ],
        },
    );
    agents.insert(
        "tech_support".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.0-flash".to_string()),
            instruction: "Handle technical issues.".to_string(),
            tools: vec![],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );
    agents.insert(
        "billing_support".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.0-flash".to_string()),
            instruction: "Handle billing issues.".to_string(),
            tools: vec![],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );
    agents.insert(
        "general_support".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.0-flash".to_string()),
            instruction: "Handle general questions.".to_string(),
            tools: vec![],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );

    let edges = vec![
        Edge::new("START", "router"),
        Edge::new("router", "tech_support"),
        Edge::new("router", "billing_support"),
        Edge::new("router", "general_support"),
        Edge::new("tech_support", "END"),
        Edge::new("billing_support", "END"),
        Edge::new("general_support", "END"),
    ];

    let project = create_template_project("Support Router", agents, edges);
    let result = generate_rust_project(&project);
    assert!(result.is_ok(), "Support router template should generate valid code");

    let generated = result.unwrap();
    let main_rs = generated.files.iter().find(|f| f.path == "src/main.rs").unwrap();
    assert!(main_rs.content.contains("Router"));
    assert!(main_rs.content.contains("classification"));
    assert!(main_rs.content.contains("technical"));
    assert!(main_rs.content.contains("billing"));
}

#[test]
fn template_web_researcher_generates_code() {
    let mut agents = HashMap::new();
    agents.insert(
        "web_agent".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.0-flash".to_string()),
            instruction: "Research using browser.".to_string(),
            tools: vec!["browser".to_string()],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );

    let edges = vec![Edge::new("START", "web_agent"), Edge::new("web_agent", "END")];

    let project = create_template_project("Web Researcher", agents, edges);
    let result = generate_rust_project(&project);
    assert!(result.is_ok(), "Web researcher template should generate valid code");

    let generated = result.unwrap();
    let main_rs = generated.files.iter().find(|f| f.path == "src/main.rs").unwrap();
    assert!(main_rs.content.contains("BrowserSession"));
    assert!(main_rs.content.contains("BrowserToolset"));
}

#[test]
fn template_writing_team_generates_code() {
    let mut agents = HashMap::new();
    agents.insert(
        "writer".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.0-flash".to_string()),
            instruction: "Write content.".to_string(),
            tools: vec![],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );
    agents.insert(
        "editor".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.0-flash".to_string()),
            instruction: "Edit content.".to_string(),
            tools: vec![],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );
    agents.insert(
        "fact_checker".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.0-flash".to_string()),
            instruction: "Check facts.".to_string(),
            tools: vec!["google_search".to_string()],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );
    agents.insert(
        "writing_team".to_string(),
        AgentSchema {
            agent_type: AgentType::Sequential,
            model: None,
            instruction: String::new(),
            tools: vec![],
            sub_agents: vec![
                "writer".to_string(),
                "editor".to_string(),
                "fact_checker".to_string(),
            ],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );

    let edges = vec![Edge::new("START", "writing_team"), Edge::new("writing_team", "END")];

    let project = create_template_project("Writing Team", agents, edges);
    let result = generate_rust_project(&project);
    assert!(result.is_ok(), "Writing team template should generate valid code");

    let generated = result.unwrap();
    let main_rs = generated.files.iter().find(|f| f.path == "src/main.rs").unwrap();
    assert!(main_rs.content.contains("SequentialAgent"));
    assert!(main_rs.content.contains("writer_agent"));
    assert!(main_rs.content.contains("editor_agent"));
    assert!(main_rs.content.contains("fact_checker_agent"));
}

#[test]
fn template_eval_loop_generates_code() {
    let mut agents = HashMap::new();
    agents.insert(
        "generator".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.0-flash".to_string()),
            instruction: "Generate response.".to_string(),
            tools: vec![],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );
    agents.insert(
        "evaluator".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.0-flash".to_string()),
            instruction: "Evaluate response.".to_string(),
            tools: vec!["exit_loop".to_string()],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );
    agents.insert(
        "eval_loop".to_string(),
        AgentSchema {
            agent_type: AgentType::Loop,
            model: None,
            instruction: String::new(),
            tools: vec![],
            sub_agents: vec!["generator".to_string(), "evaluator".to_string()],
            position: Default::default(),
            max_iterations: Some(3),
            routes: vec![],
        },
    );

    let edges = vec![Edge::new("START", "eval_loop"), Edge::new("eval_loop", "END")];

    let project = create_template_project("Eval Loop", agents, edges);
    let result = generate_rust_project(&project);
    assert!(result.is_ok(), "Eval loop template should generate valid code");

    let generated = result.unwrap();
    let main_rs = generated.files.iter().find(|f| f.path == "src/main.rs").unwrap();
    assert!(main_rs.content.contains("LoopAgent"));
    assert!(main_rs.content.contains("ExitLoopTool"));
}

#[test]
fn template_voice_assistant_generates_code() {
    let mut agents = HashMap::new();
    agents.insert(
        "voice_agent".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.0-flash".to_string()),
            instruction: "Respond naturally for voice.".to_string(),
            tools: vec![],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );

    let edges = vec![Edge::new("START", "voice_agent"), Edge::new("voice_agent", "END")];

    let project = create_template_project("Voice Assistant", agents, edges);
    let result = generate_rust_project(&project);
    assert!(result.is_ok(), "Voice assistant template should generate valid code");
}

#[test]
fn template_realtime_translator_generates_code() {
    let mut agents = HashMap::new();
    agents.insert(
        "translator".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.0-flash".to_string()),
            instruction: "Translate in real-time.".to_string(),
            tools: vec![],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );

    let edges = vec![Edge::new("START", "translator"), Edge::new("translator", "END")];

    let project = create_template_project("Realtime Translator", agents, edges);
    let result = generate_rust_project(&project);
    assert!(result.is_ok(), "Realtime translator template should generate valid code");
}
