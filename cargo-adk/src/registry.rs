//! Template Registry — central data store for all templates, addons, and patterns.
//!
//! The registry is populated at startup with all built-in templates and can be
//! extended with custom templates loaded from a directory.

use std::collections::HashMap;
use std::path::Path;

use crate::addon::{AddonCodeFragments, CapabilityAddon};
use crate::pattern::EnterprisePattern;
use crate::template::{AgentCodeFragments, AgentTemplate, TemplateCategory};

/// Central registry of all available templates, addons, and patterns.
#[derive(Debug, Clone)]
pub struct TemplateRegistry {
    /// All registered agent templates.
    pub agent_templates: Vec<AgentTemplate>,
    /// All registered capability addons.
    pub capability_addons: Vec<CapabilityAddon>,
    /// All registered enterprise patterns.
    pub enterprise_patterns: Vec<EnterprisePattern>,
    /// Alias mappings (e.g., "basic" → "llm").
    pub aliases: HashMap<&'static str, &'static str>,
}

impl TemplateRegistry {
    /// Build the default registry with all built-in templates.
    ///
    /// Populates 8 agent templates, 9 capability addons, 5 enterprise patterns,
    /// and 6 legacy aliases.
    pub fn builtin() -> Self {
        Self {
            agent_templates: builtin_agent_templates(),
            capability_addons: builtin_capability_addons(),
            enterprise_patterns: builtin_enterprise_patterns(),
            aliases: builtin_aliases(),
        }
    }

    /// Load additional templates from a directory.
    pub fn load_custom_dir(&mut self, _dir: &Path) -> Result<(), String> {
        // Will be implemented in a later task
        Ok(())
    }

    /// Resolve a template name (handling aliases).
    pub fn resolve_template(&self, name: &str) -> Option<&AgentTemplate> {
        let resolved_name = self.aliases.get(name).copied().unwrap_or(name);
        self.agent_templates.iter().find(|t| t.name == resolved_name)
    }

    /// Get compatible addons for a given template.
    pub fn compatible_addons(&self, template: &str) -> Vec<&CapabilityAddon> {
        let tmpl = match self.resolve_template(template) {
            Some(t) => t,
            None => return Vec::new(),
        };

        self.capability_addons
            .iter()
            .filter(|addon| {
                // Addon is compatible if:
                // 1. Template doesn't list it as incompatible
                !tmpl.incompatible_addons.contains(&addon.name)
                // 2. Addon doesn't list the template as incompatible
                    && !addon.incompatible_with.contains(&tmpl.name)
            })
            .collect()
    }

    /// Resolve an enterprise pattern into its definition.
    pub fn resolve_pattern(&self, name: &str) -> Option<&EnterprisePattern> {
        self.enterprise_patterns.iter().find(|p| p.name == name)
    }
}

// ---------------------------------------------------------------------------
// Built-in data population
// ---------------------------------------------------------------------------

/// All 8 built-in agent templates.
fn builtin_agent_templates() -> Vec<AgentTemplate> {
    vec![
        AgentTemplate {
            name: "llm",
            description: "Single LLM agent with tool calling support",
            category: TemplateCategory::AgentType,
            default_provider: "gemini",
            required_features: vec!["minimal"],
            incompatible_addons: vec![],
            code_fragments: AgentCodeFragments {
                imports: vec!["use std::sync::Arc;"],
                agent_construction: r#"let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("{name}")
            .description("An AI assistant")
            .instruction("You are a helpful assistant.")
            .model(Arc::new(model))
            .build()?,
    );"#,
                additional_files: vec![],
            },
        },
        AgentTemplate {
            name: "sequential",
            description: "Sequential multi-agent pipeline executing agents in order",
            category: TemplateCategory::AgentType,
            default_provider: "gemini",
            required_features: vec!["minimal"],
            incompatible_addons: vec![],
            code_fragments: AgentCodeFragments {
                imports: vec!["use std::sync::Arc;", "use adk_rust::agents::SequentialAgent;"],
                agent_construction: r#"let researcher = Arc::new(
        LlmAgentBuilder::new("researcher")
            .description("Research agent")
            .instruction("Research the given topic thoroughly.")
            .model(Arc::new(model.clone()))
            .build()?,
    );

    let writer = Arc::new(
        LlmAgentBuilder::new("writer")
            .description("Writing agent")
            .instruction("Write a clear summary based on the research.")
            .model(Arc::new(model))
            .build()?,
    );

    let agent: Arc<dyn Agent> = Arc::new(
        SequentialAgent::new("{name}", vec![researcher, writer]),
    );"#,
                additional_files: vec![],
            },
        },
        AgentTemplate {
            name: "parallel",
            description: "Parallel multi-agent execution with result aggregation",
            category: TemplateCategory::AgentType,
            default_provider: "gemini",
            required_features: vec!["minimal"],
            incompatible_addons: vec![],
            code_fragments: AgentCodeFragments {
                imports: vec!["use std::sync::Arc;", "use adk_rust::agents::ParallelAgent;"],
                agent_construction: r#"let analyst = Arc::new(
        LlmAgentBuilder::new("analyst")
            .description("Data analyst")
            .instruction("Analyze the data and provide insights.")
            .model(Arc::new(model.clone()))
            .build()?,
    );

    let reviewer = Arc::new(
        LlmAgentBuilder::new("reviewer")
            .description("Quality reviewer")
            .instruction("Review the analysis for accuracy.")
            .model(Arc::new(model))
            .build()?,
    );

    let agent: Arc<dyn Agent> = Arc::new(
        ParallelAgent::new("{name}", vec![analyst, reviewer]),
    );"#,
                additional_files: vec![],
            },
        },
        AgentTemplate {
            name: "loop",
            description: "Loop agent that iterates until a condition is met",
            category: TemplateCategory::AgentType,
            default_provider: "gemini",
            required_features: vec!["minimal"],
            incompatible_addons: vec![],
            code_fragments: AgentCodeFragments {
                imports: vec!["use std::sync::Arc;", "use adk_rust::agents::LoopAgent;"],
                agent_construction: r#"let worker = Arc::new(
        LlmAgentBuilder::new("worker")
            .description("Iterative worker")
            .instruction("Refine the output. When satisfied, respond with DONE.")
            .model(Arc::new(model))
            .build()?,
    );

    let agent: Arc<dyn Agent> = Arc::new(
        LoopAgent::builder()
            .name("{name}")
            .agent(worker)
            .max_iterations(5)
            .build(),
    );"#,
                additional_files: vec![],
            },
        },
        AgentTemplate {
            name: "conditional",
            description: "Conditional agent that routes based on LLM decisions",
            category: TemplateCategory::AgentType,
            default_provider: "gemini",
            required_features: vec!["minimal"],
            incompatible_addons: vec![],
            code_fragments: AgentCodeFragments {
                imports: vec!["use std::sync::Arc;", "use adk_rust::agents::ConditionalAgent;"],
                agent_construction: r#"let technical = Arc::new(
        LlmAgentBuilder::new("technical")
            .description("Technical expert")
            .instruction("Provide detailed technical answers.")
            .model(Arc::new(model.clone()))
            .build()?,
    );

    let general = Arc::new(
        LlmAgentBuilder::new("general")
            .description("General assistant")
            .instruction("Provide helpful general answers.")
            .model(Arc::new(model))
            .build()?,
    );

    let agent: Arc<dyn Agent> = Arc::new(
        ConditionalAgent::new("{name}", vec![technical, general]),
    );"#,
                additional_files: vec![],
            },
        },
        AgentTemplate {
            name: "graph",
            description: "Graph-based workflow with checkpoints and durable execution",
            category: TemplateCategory::AgentType,
            default_provider: "gemini",
            required_features: vec!["minimal", "graph"],
            incompatible_addons: vec![],
            code_fragments: AgentCodeFragments {
                imports: vec!["use std::sync::Arc;", "use adk_rust::graph::*;"],
                agent_construction: r#"// Graph-based workflow with checkpoints
    // See adk-graph documentation for full API
    let agent = LlmAgentBuilder::new("{name}")
        .description("A graph-based workflow agent")
        .instruction("You orchestrate a multi-step workflow.")
        .model(Arc::new(model))
        .build()?;
    let agent: Arc<dyn Agent> = Arc::new(agent);"#,
                additional_files: vec![],
            },
        },
        AgentTemplate {
            name: "realtime",
            description: "Real-time bidirectional audio/video streaming agent",
            category: TemplateCategory::AgentType,
            default_provider: "gemini",
            required_features: vec!["minimal", "realtime"],
            incompatible_addons: vec![],
            code_fragments: AgentCodeFragments {
                imports: vec!["use std::sync::Arc;", "use adk_rust::realtime::*;"],
                agent_construction: r#"// Real-time voice agent with bidirectional audio
    // See adk-realtime documentation for full API
    let agent = LlmAgentBuilder::new("{name}")
        .description("A real-time voice assistant")
        .instruction("You are a voice assistant. Respond naturally and concisely.")
        .model(Arc::new(model))
        .build()?;
    let agent: Arc<dyn Agent> = Arc::new(agent);"#,
                additional_files: vec![],
            },
        },
        AgentTemplate {
            name: "custom",
            description: "Custom agent with manual trait implementation",
            category: TemplateCategory::AgentType,
            default_provider: "gemini",
            required_features: vec!["minimal"],
            incompatible_addons: vec![],
            code_fragments: AgentCodeFragments {
                imports: vec!["use std::sync::Arc;", "use async_trait::async_trait;"],
                agent_construction: r#"// Custom agent implementing the Agent trait directly
    struct MyAgent;

    #[async_trait]
    impl Agent for MyAgent {
        fn name(&self) -> &str { "{name}" }
        fn description(&self) -> &str { "A custom agent" }
        fn sub_agents(&self) -> &[Arc<dyn Agent>] { &[] }
        async fn run(&self, ctx: Arc<dyn InvocationContext>) -> adk_rust::prelude::Result<EventStream> {
            // Your custom logic here
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    let agent: Arc<dyn Agent> = Arc::new(MyAgent);"#,
                additional_files: vec![],
            },
        },
    ]
}

/// All 9 built-in capability addons.
fn builtin_capability_addons() -> Vec<CapabilityAddon> {
    vec![
        CapabilityAddon {
            name: "telemetry",
            description: "OpenTelemetry tracing integration",
            required_features: vec!["telemetry"],
            additional_deps: vec![],
            init_priority: 10,
            incompatible_with: vec![],
            code_fragments: AddonCodeFragments {
                imports: vec![],
                initialization: "",
                agent_builder_calls: "",
                env_vars: vec![(
                    "OTEL_EXPORTER_OTLP_ENDPOINT",
                    "OpenTelemetry collector endpoint (optional)",
                )],
                additional_files: vec![],
            },
        },
        CapabilityAddon {
            name: "auth",
            description: "API key and JWT authentication",
            required_features: vec!["auth"],
            additional_deps: vec![],
            init_priority: 20,
            incompatible_with: vec![],
            code_fragments: AddonCodeFragments {
                imports: vec![],
                initialization: r#"let _auth_key = std::env::var("AUTH_API_KEY")
        .expect("AUTH_API_KEY must be set for authentication");
    tracing::info!("auth key loaded");"#,
                agent_builder_calls: "",
                env_vars: vec![("AUTH_API_KEY", "API key for request authentication")],
                additional_files: vec![],
            },
        },
        CapabilityAddon {
            name: "sessions",
            description: "Session state management and persistence",
            required_features: vec!["sessions"],
            additional_deps: vec![],
            init_priority: 30,
            incompatible_with: vec![],
            code_fragments: AddonCodeFragments {
                imports: vec!["use adk_rust::session::InMemorySessionService;"],
                initialization: r#"let _session_service = Arc::new(InMemorySessionService::new());
    tracing::info!("session service initialized");
    // Wire into runner: Runner::builder().session_service(_session_service)
    // For PostgreSQL: PostgresSessionService::connect(&database_url).await?
    // For Redis: RedisSessionService::connect(&redis_url).await?"#,
                agent_builder_calls: "",
                env_vars: vec![(
                    "DATABASE_URL",
                    "Session database URL (optional, uses in-memory by default)",
                )],
                additional_files: vec![],
            },
        },
        CapabilityAddon {
            name: "memory",
            description: "Semantic memory and RAG search integration",
            required_features: vec!["memory"],
            additional_deps: vec![],
            init_priority: 40,
            incompatible_with: vec![],
            code_fragments: AddonCodeFragments {
                imports: vec!["use adk_rust::memory::InMemoryMemoryService;"],
                initialization: r#"let _memory_service = Arc::new(InMemoryMemoryService::new());
    tracing::info!("memory service initialized");
    // Wire into runner: Runner::builder().memory_service(_memory_service)"#,
                agent_builder_calls: "",
                env_vars: vec![],
                additional_files: vec![],
            },
        },
        CapabilityAddon {
            name: "mcp",
            description: "Model Context Protocol server connections",
            required_features: vec!["tools", "mcp"],
            additional_deps: vec![],
            init_priority: 50,
            incompatible_with: vec![],
            code_fragments: AddonCodeFragments {
                imports: vec!["use adk_rust::tool::McpToolset;"],
                initialization: r#"// Connect to MCP servers for extended tool capabilities
    // let mcp_tools = McpToolset::from_server("npx", &["-y", "@anthropic/mcp-server-filesystem", "/tmp"]).await?;
    tracing::info!("MCP tools ready");"#,
                agent_builder_calls: "",
                env_vars: vec![],
                additional_files: vec![],
            },
        },
        CapabilityAddon {
            name: "guardrails",
            description: "Input/output validation and content filtering",
            required_features: vec!["guardrail"],
            additional_deps: vec![],
            init_priority: 60,
            incompatible_with: vec![],
            code_fragments: AddonCodeFragments {
                imports: vec!["use adk_rust::guardrail::*;"],
                initialization: r#"// Configure input/output guardrails
    // let guardrail = PiiRedactionGuardrail::new();
    tracing::info!("guardrails configured");"#,
                agent_builder_calls: "",
                env_vars: vec![],
                additional_files: vec![],
            },
        },
        CapabilityAddon {
            name: "eval",
            description: "Evaluation framework for agent quality testing",
            required_features: vec!["eval"],
            additional_deps: vec![],
            init_priority: 70,
            incompatible_with: vec![],
            code_fragments: AddonCodeFragments {
                imports: vec!["use adk_rust::eval::*;"],
                initialization: r#"// Evaluation harness for agent quality testing
    // Run with: cargo test -- --ignored
    tracing::info!("eval framework available");"#,
                agent_builder_calls: "",
                env_vars: vec![],
                additional_files: vec![],
            },
        },
        CapabilityAddon {
            name: "browser",
            description: "Browser automation tools via WebDriver",
            required_features: vec!["browser"],
            additional_deps: vec![],
            init_priority: 80,
            incompatible_with: vec![],
            code_fragments: AddonCodeFragments {
                imports: vec!["use adk_rust::browser::BrowserTool;"],
                initialization: r#"// Browser automation tool via WebDriver
    // let browser_tool = BrowserTool::new().await?;
    tracing::info!("browser tools available");"#,
                agent_builder_calls: "",
                env_vars: vec![("WEBDRIVER_URL", "WebDriver endpoint URL (optional)")],
                additional_files: vec![],
            },
        },
        CapabilityAddon {
            name: "server",
            description: "HTTP server with A2A protocol support",
            required_features: vec!["server"],
            additional_deps: vec![],
            init_priority: 90,
            incompatible_with: vec![],
            code_fragments: AddonCodeFragments {
                imports: vec!["use adk_rust::server::A2aServer;"],
                initialization: r#"let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let addr = format!("0.0.0.0:{port}");

    let server = A2aServer::builder()
        .agent(agent.clone())
        .bind_addr(&addr)
        .build()?;

    tracing::info!("starting A2A server on http://{}", addr);
    server.serve().await?;"#,
                agent_builder_calls: "",
                env_vars: vec![("PORT", "Server port (default: 8080)")],
                additional_files: vec![],
            },
        },
    ]
}

/// All 5 built-in enterprise patterns.
fn builtin_enterprise_patterns() -> Vec<EnterprisePattern> {
    vec![
        EnterprisePattern {
            name: "multi-agent",
            description: "Multi-agent supervisor with telemetry observability",
            base_template: "sequential",
            included_addons: vec!["telemetry"],
            override_features: None,
            code_fragments: None,
        },
        EnterprisePattern {
            name: "production",
            description: "Production-ready LLM agent with server, auth, sessions, and telemetry",
            base_template: "llm",
            included_addons: vec!["server", "auth", "sessions", "telemetry"],
            override_features: None,
            code_fragments: None,
        },
        EnterprisePattern {
            name: "pipeline",
            description: "Sequential data processing pipeline with session state",
            base_template: "sequential",
            included_addons: vec!["sessions", "telemetry"],
            override_features: None,
            code_fragments: None,
        },
        EnterprisePattern {
            name: "chatbot",
            description: "Conversational chatbot with memory and HTTP interface",
            base_template: "llm",
            included_addons: vec!["sessions", "memory", "server"],
            override_features: None,
            code_fragments: None,
        },
        EnterprisePattern {
            name: "a2a-server",
            description: "Agent-to-Agent protocol server with session management",
            base_template: "llm",
            included_addons: vec!["server", "sessions"],
            override_features: None,
            code_fragments: None,
        },
    ]
}

/// Legacy alias mappings (6 total).
fn builtin_aliases() -> HashMap<&'static str, &'static str> {
    let mut aliases = HashMap::new();
    aliases.insert("basic", "llm");
    aliases.insert("tools", "llm");
    aliases.insert("rag", "llm");
    aliases.insert("api", "llm");
    aliases.insert("openai", "llm");
    aliases.insert("a2a", "llm");
    aliases
}
