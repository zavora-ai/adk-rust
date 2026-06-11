//! Template Registry — central data store for all templates, addons, and patterns.
//!
//! The registry is populated at startup with all built-in templates and can be
//! extended with custom templates loaded from a directory.

use std::collections::HashMap;
use std::path::Path;

use crate::addon::{AddonCodeFragments, CapabilityAddon, DependencySpec};
use crate::codegen::ADK_VERSION;
use crate::pattern::EnterprisePattern;
use crate::template::{AgentCodeFragments, AgentTemplate, FileFragment, TemplateCategory};

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
    /// Populates 12 agent templates, 9 capability addons, 5 enterprise patterns,
    /// and 2 legacy aliases.
    pub fn builtin() -> Self {
        Self {
            agent_templates: builtin_agent_templates(),
            capability_addons: builtin_capability_addons(),
            enterprise_patterns: builtin_enterprise_patterns(),
            aliases: builtin_aliases(),
        }
    }

    /// Load additional templates from a directory of TOML manifests.
    ///
    /// Each `*.toml` file describes one template:
    ///
    /// ```toml
    /// name = "my-template"                   # required
    /// description = "My custom agent"        # optional
    /// provider = "gemini"                    # optional, default provider
    /// features = ["minimal", "tools"]        # optional, adk-rust features
    /// imports = ["use std::sync::Arc;"]      # optional, main.rs imports
    /// # optional; falls back to a basic LLM agent when omitted
    /// agent_construction = '''
    /// let agent: Arc<dyn Agent> = Arc::new(
    ///     LlmAgentBuilder::new("{name}").model(Arc::new(model)).build()?,
    /// );
    /// '''
    /// ```
    ///
    /// A custom template with the same name as a built-in replaces it.
    pub fn load_custom_dir(&mut self, dir: &Path) -> Result<(), String> {
        let entries = std::fs::read_dir(dir)
            .map_err(|e| format!("failed to read template directory '{}': {e}", dir.display()))?;

        let mut loaded = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "toml") {
                continue;
            }
            let content = std::fs::read_to_string(&path)
                .map_err(|e| format!("failed to read '{}': {e}", path.display()))?;
            let template = parse_custom_template(&content)
                .map_err(|e| format!("invalid template manifest '{}': {e}", path.display()))?;

            // Same-name custom templates replace built-ins.
            self.agent_templates.retain(|t| t.name != template.name);
            self.agent_templates.push(template);
            loaded += 1;
        }

        if loaded == 0 {
            return Err(format!("no .toml template manifests found in '{}'", dir.display()));
        }
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

    /// Resolve an enterprise pattern into its definition (handling aliases).
    pub fn resolve_pattern(&self, name: &str) -> Option<&EnterprisePattern> {
        let resolved_name = self.aliases.get(name).copied().unwrap_or(name);
        self.enterprise_patterns.iter().find(|p| p.name == resolved_name)
    }
}

/// Parse a custom template TOML manifest into an [`AgentTemplate`].
///
/// Strings are leaked to satisfy the `&'static str` fields; the CLI is a
/// short-lived process, so this is bounded and acceptable (the JSON template
/// listing uses the same approach).
fn parse_custom_template(content: &str) -> Result<AgentTemplate, String> {
    fn leak(s: &str) -> &'static str {
        Box::leak(s.to_string().into_boxed_str())
    }

    let value: toml::Value = content.parse().map_err(|e| format!("TOML parse error: {e}"))?;

    let name = value.get("name").and_then(|v| v.as_str()).ok_or("missing required 'name' field")?;
    let description =
        value.get("description").and_then(|v| v.as_str()).unwrap_or("Custom agent template");
    let default_provider = value.get("provider").and_then(|v| v.as_str()).unwrap_or("gemini");

    let required_features: Vec<&'static str> = value
        .get("features")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|f| f.as_str()).map(leak).collect())
        .unwrap_or_else(|| vec!["minimal"]);

    let imports: Vec<&'static str> = value
        .get("imports")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|f| f.as_str()).map(leak).collect())
        .unwrap_or_else(|| vec!["use std::sync::Arc;"]);

    // Empty construction falls back to the basic LLM agent in codegen.
    let agent_construction =
        value.get("agent_construction").and_then(|v| v.as_str()).map(leak).unwrap_or("");

    Ok(AgentTemplate {
        name: leak(name),
        description: leak(description),
        category: TemplateCategory::AgentType,
        default_provider: leak(default_provider),
        required_features,
        incompatible_addons: vec![],
        additional_deps: vec![],
        code_fragments: AgentCodeFragments {
            imports,
            agent_construction,
            additional_files: vec![],
        },
    })
}

// ---------------------------------------------------------------------------
// Built-in data population
// ---------------------------------------------------------------------------

/// All 12 built-in agent templates.
fn builtin_agent_templates() -> Vec<AgentTemplate> {
    vec![
        AgentTemplate {
            name: "llm",
            description: "Single LLM agent with tool calling support",
            category: TemplateCategory::AgentType,
            default_provider: "gemini",
            required_features: vec!["minimal"],
            incompatible_addons: vec![],
            additional_deps: vec![],
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
            additional_deps: vec![],
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
            additional_deps: vec![],
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
            additional_deps: vec![],
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
            additional_deps: vec![],
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
            additional_deps: vec![],
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
            additional_deps: vec![],
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
            additional_deps: vec![],
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
        AgentTemplate {
            name: "tools",
            description: "LLM agent with #[tool] custom tools",
            category: TemplateCategory::AgentType,
            default_provider: "gemini",
            required_features: vec!["minimal", "tools"],
            incompatible_addons: vec![],
            additional_deps: vec![
                DependencySpec { crate_name: "adk-tool", version: ADK_VERSION, features: vec![] },
                DependencySpec { crate_name: "serde", version: "1", features: vec!["derive"] },
                DependencySpec { crate_name: "serde_json", version: "1", features: vec![] },
                DependencySpec { crate_name: "schemars", version: "1", features: vec![] },
            ],
            code_fragments: AgentCodeFragments {
                imports: vec!["use std::sync::Arc;", "mod tools;", "use tools::Greet;"],
                agent_construction: r#"let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("{name}")
            .description("Assistant with custom tools")
            .instruction("You are a helpful assistant. Use the greet tool when asked to greet someone.")
            .model(Arc::new(model))
            .tool(Arc::new(Greet))
            .build()?,
    );"#,
                additional_files: vec![FileFragment {
                    path: "src/tools.rs",
                    content: r#"//! Custom tools exposed to the agent via the `#[tool]` macro.

use adk_tool::{AdkError, tool};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize, JsonSchema)]
pub struct GreetArgs {
    /// Name of the person to greet
    pub name: String,
    /// Greeting style: formal or casual
    pub style: Option<String>,
}

/// Greet a person by name.
#[tool]
pub async fn greet(args: GreetArgs) -> std::result::Result<Value, AdkError> {
    let greeting = match args.style.as_deref() {
        Some("formal") => format!("Good day, {}. How may I assist you?", args.name),
        _ => format!("Hey {}! What's up?", args.name),
    };
    Ok(json!({ "greeting": greeting }))
}
"#,
                }],
            },
        },
        AgentTemplate {
            name: "rag",
            description: "RAG agent with vector search over a knowledge base",
            category: TemplateCategory::AgentType,
            default_provider: "gemini",
            required_features: vec!["minimal"],
            incompatible_addons: vec![],
            additional_deps: vec![DependencySpec {
                crate_name: "adk-rag",
                version: ADK_VERSION,
                features: vec!["gemini"],
            }],
            code_fragments: AgentCodeFragments {
                imports: vec![
                    "use std::sync::Arc;",
                    "use adk_rag::{Document, FixedSizeChunker, GeminiEmbeddingProvider, InMemoryVectorStore, RagConfig, RagPipeline, RagTool};",
                ],
                agent_construction: r#"// Embeddings use Gemini; set GOOGLE_API_KEY for the embedding provider.
    let gemini_key = std::env::var("GOOGLE_API_KEY")
        .map_err(|_| anyhow::anyhow!("GOOGLE_API_KEY is required for Gemini embeddings — copy .env.example to .env and add your key"))?;

    let pipeline = Arc::new(
        RagPipeline::builder()
            .config(RagConfig::default())
            .embedding_provider(Arc::new(GeminiEmbeddingProvider::new(&gemini_key)?))
            .vector_store(Arc::new(InMemoryVectorStore::new()))
            .chunker(Arc::new(FixedSizeChunker::new(256, 50)))
            .build()?,
    );

    pipeline.create_collection("docs").await?;
    pipeline.ingest("docs", &Document {
        id: "example".into(),
        text: "ADK-Rust is a framework for building AI agents in Rust. \
               It supports multiple LLM providers, tool calling, RAG, and more.".into(),
        metadata: Default::default(),
        source_uri: None,
    }).await?;

    tracing::info!("ingested example document into the 'docs' collection");

    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("{name}")
            .description("RAG-powered knowledge assistant")
            .instruction("Use the rag_search tool to find relevant documents before answering.")
            .model(Arc::new(model))
            .tool(Arc::new(RagTool::new(pipeline, "docs")))
            .build()?,
    );"#,
                additional_files: vec![],
            },
        },
        AgentTemplate {
            name: "api",
            description: "REST API server exposing the agent over HTTP",
            category: TemplateCategory::AgentType,
            default_provider: "gemini",
            required_features: vec!["minimal", "server"],
            incompatible_addons: vec!["server"],
            additional_deps: vec![DependencySpec {
                crate_name: "axum",
                version: "0.8",
                features: vec![],
            }],
            code_fragments: AgentCodeFragments {
                imports: vec![
                    "use std::sync::Arc;",
                    "use adk_rust::server::{ServerConfig, create_app};",
                    "use adk_rust::session::InMemorySessionService;",
                ],
                agent_construction: r#"let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("{name}")
            .description("REST API agent")
            .instruction("You are a helpful assistant accessible via REST API.")
            .model(Arc::new(model))
            .build()?,
    );

    let session_service = Arc::new(InMemorySessionService::new());

    let config = ServerConfig::new(
        Arc::new(adk_rust::SingleAgentLoader::new(agent)),
        session_service,
    );
    let app = create_app(config);

    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let addr = format!("0.0.0.0:{port}");
    tracing::info!("ADK agent server running on http://{addr}");
    tracing::info!("  GET  /api/health                                — health check");
    tracing::info!("  POST /api/sessions                              — create a session (appName, userId)");
    tracing::info!("  POST /api/run/{name}/<user_id>/<session_id>     — send a message, SSE response");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;"#,
                additional_files: vec![],
            },
        },
        AgentTemplate {
            name: "openai",
            description: "OpenAI-powered LLM agent",
            category: TemplateCategory::AgentType,
            default_provider: "openai",
            required_features: vec!["minimal", "openai"],
            incompatible_addons: vec![],
            additional_deps: vec![],
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

/// Legacy alias mappings.
///
/// `tools`, `rag`, and `openai` are real templates and `api` is a real
/// pattern (they previously aliased to plain `llm`, silently dropping the
/// capability the name promised). `a2a` resolves to the `a2a-server` pattern.
fn builtin_aliases() -> HashMap<&'static str, &'static str> {
    let mut aliases = HashMap::new();
    aliases.insert("basic", "llm");
    aliases.insert("a2a", "a2a-server");
    aliases
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_has_advertised_templates() {
        let registry = TemplateRegistry::builtin();
        for name in ["llm", "tools", "rag", "api", "openai", "graph", "realtime"] {
            assert!(
                registry.resolve_template(name).is_some(),
                "advertised template '{name}' missing from registry"
            );
        }
        // Aliases resolve to real targets.
        assert_eq!(registry.resolve_template("basic").unwrap().name, "llm");
        assert_eq!(registry.resolve_pattern("a2a").unwrap().name, "a2a-server");
    }

    #[test]
    fn openai_template_defaults_to_openai_provider() {
        let registry = TemplateRegistry::builtin();
        assert_eq!(registry.resolve_template("openai").unwrap().default_provider, "openai");
    }

    #[test]
    fn load_custom_dir_parses_toml_manifest() {
        let dir = std::env::temp_dir().join("cargo-adk-custom-templates-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("greeter.toml"),
            r#"
name = "greeter"
description = "A custom greeter agent"
provider = "gemini"
features = ["minimal"]
"#,
        )
        .unwrap();

        let mut registry = TemplateRegistry::builtin();
        registry.load_custom_dir(&dir).unwrap();

        let tmpl = registry.resolve_template("greeter").expect("custom template loaded");
        assert_eq!(tmpl.description, "A custom greeter agent");
        assert_eq!(tmpl.default_provider, "gemini");
        // Empty construction means codegen falls back to the basic LLM agent.
        assert!(tmpl.code_fragments.agent_construction.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_custom_dir_rejects_empty_dir() {
        let dir = std::env::temp_dir().join("cargo-adk-custom-templates-empty");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut registry = TemplateRegistry::builtin();
        assert!(registry.load_custom_dir(&dir).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
