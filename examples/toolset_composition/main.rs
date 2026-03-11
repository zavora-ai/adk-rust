//! Toolset Composition Example
//!
//! Demonstrates the reusable toolset wrappers from the browser production
//! hardening spec: FilteredToolset, MergedToolset, and PrefixedToolset.
//! These utilities work with any `Toolset` implementation.
//!
//! ## Features Showcased
//!
//! - `FilteredToolset` — filter tools from any toolset by predicate
//! - `MergedToolset` — combine multiple toolsets into one
//! - `PrefixedToolset` — namespace tool names with a prefix
//! - `BasicToolset` — group static tools together
//! - `string_predicate` — allow-list filter by tool name
//! - `LlmAgentBuilder::toolset()` — register composed toolsets
//!
//! ## Requirements
//!
//! `GOOGLE_API_KEY` environment variable
//!
//! ## Running
//!
//! ```bash
//! cargo run --example toolset_composition
//! ```

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, CallbackContext, Content, InvocationContext, Part, ReadonlyContext, Result as AdkResult,
    RunConfig, Session, State, Tool, ToolContext, Toolset,
};
use adk_model::GeminiModel;
use adk_tool::{
    BasicToolset, FilteredToolset, FunctionTool, MergedToolset, PrefixedToolset, string_predicate,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Sample tools
// ---------------------------------------------------------------------------

async fn get_weather(_ctx: Arc<dyn ToolContext>, args: Value) -> AdkResult<Value> {
    let city = args["city"].as_str().unwrap_or("unknown");
    Ok(json!({ "city": city, "temp": 72, "condition": "sunny" }))
}

async fn get_forecast(_ctx: Arc<dyn ToolContext>, args: Value) -> AdkResult<Value> {
    let city = args["city"].as_str().unwrap_or("unknown");
    Ok(json!({ "city": city, "forecast": ["sunny", "cloudy", "rain"] }))
}

async fn search_web(_ctx: Arc<dyn ToolContext>, args: Value) -> AdkResult<Value> {
    let query = args["query"].as_str().unwrap_or("unknown");
    Ok(json!({ "results": [format!("Result 1 for '{query}'"), format!("Result 2 for '{query}'")] }))
}

async fn calculate(_ctx: Arc<dyn ToolContext>, args: Value) -> AdkResult<Value> {
    let expr = args["expression"].as_str().unwrap_or("0");
    Ok(json!({ "result": format!("{expr} = 42") }))
}

async fn translate(_ctx: Arc<dyn ToolContext>, args: Value) -> AdkResult<Value> {
    let text = args["text"].as_str().unwrap_or("");
    let to = args["to"].as_str().unwrap_or("es");
    Ok(json!({ "translated": format!("[{to}] {text}"), "language": to }))
}

// ---------------------------------------------------------------------------
// Minimal session / context boilerplate
// ---------------------------------------------------------------------------

struct SimpleState(std::sync::Mutex<HashMap<String, Value>>);
impl SimpleState {
    fn new() -> Self {
        Self(std::sync::Mutex::new(HashMap::new()))
    }
}
impl State for SimpleState {
    fn get(&self, key: &str) -> Option<Value> {
        self.0.lock().unwrap().get(key).cloned()
    }
    fn set(&mut self, key: String, value: Value) {
        self.0.lock().unwrap().insert(key, value);
    }
    fn all(&self) -> HashMap<String, Value> {
        self.0.lock().unwrap().clone()
    }
}
struct Sess(SimpleState);
impl Session for Sess {
    fn id(&self) -> &str {
        "s1"
    }
    fn app_name(&self) -> &str {
        "compose"
    }
    fn user_id(&self) -> &str {
        "user"
    }
    fn state(&self) -> &dyn State {
        &self.0
    }
    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
}
struct Ctx {
    agent: Arc<dyn Agent>,
    content: Content,
    config: RunConfig,
    session: Sess,
}
#[async_trait]
impl ReadonlyContext for Ctx {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        self.agent.name()
    }
    fn user_id(&self) -> &str {
        "user"
    }
    fn app_name(&self) -> &str {
        "compose"
    }
    fn session_id(&self) -> &str {
        "s1"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}
#[async_trait]
impl CallbackContext for Ctx {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}
#[async_trait]
impl InvocationContext for Ctx {
    fn agent(&self) -> Arc<dyn Agent> {
        self.agent.clone()
    }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }
    fn session(&self) -> &dyn Session {
        &self.session
    }
    fn run_config(&self) -> &RunConfig {
        &self.config
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
}

async fn run_agent(
    agent: Arc<dyn Agent>,
    task: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let content =
        Content { role: "user".to_string(), parts: vec![Part::Text { text: task.to_string() }] };
    let ctx = Arc::new(Ctx {
        agent: agent.clone(),
        content,
        config: RunConfig::default(),
        session: Sess(SimpleState::new()),
    });
    let mut stream = agent.run(ctx).await?;
    let mut response = String::new();
    while let Some(result) = stream.next().await {
        match result {
            Ok(event) => {
                if let Some(c) = &event.llm_response.content {
                    for part in &c.parts {
                        if let Part::Text { text } = part {
                            response.push_str(text);
                        }
                    }
                }
            }
            Err(e) => return Err(format!("agent error: {e}").into()),
        }
    }
    Ok(response)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let _ = dotenvy::dotenv();

    println!("=== Toolset Composition Example ===\n");

    let api_key = match std::env::var("GOOGLE_API_KEY") {
        Ok(k) => k,
        Err(_) => {
            println!("GOOGLE_API_KEY not set. export GOOGLE_API_KEY=your_key");
            return Ok(());
        }
    };
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // --- Build some toolsets -------------------------------------------------

    let weather_tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(FunctionTool::new("get_weather", "Get current weather for a city", get_weather)),
        Arc::new(FunctionTool::new("get_forecast", "Get 3-day forecast for a city", get_forecast)),
    ];

    let utility_tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(FunctionTool::new("search_web", "Search the web", search_web)),
        Arc::new(FunctionTool::new("calculate", "Evaluate a math expression", calculate)),
        Arc::new(FunctionTool::new("translate", "Translate text to another language", translate)),
    ];

    let weather_toolset = Arc::new(BasicToolset::new("weather", weather_tools));
    let utility_toolset = Arc::new(BasicToolset::new("utilities", utility_tools));

    // =========================================================================
    // Example 1: FilteredToolset — allow only specific tools
    // =========================================================================
    println!("--- Example 1: FilteredToolset ---");
    println!("  Filtering weather toolset to only expose 'get_weather' (no forecast)\n");

    let filtered =
        FilteredToolset::new(weather_toolset.clone(), string_predicate(vec!["get_weather".into()]));

    // Verify filtering works
    let dummy_ctx: Arc<dyn ReadonlyContext> = Arc::new(DummyCtx);
    let tools = filtered.tools(dummy_ctx.clone()).await?;
    println!("  Filtered tools: {:?}", tools.iter().map(|t| t.name()).collect::<Vec<_>>());

    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("filtered_agent")
            .model(model.clone())
            .instruction("Use available tools to answer weather questions.")
            .toolset(Arc::new(filtered))
            .build()?,
    );

    let resp = run_agent(agent, "What's the weather in Tokyo?").await?;
    println!("  Response: {}\n", resp.chars().take(200).collect::<String>());

    // =========================================================================
    // Example 2: FilteredToolset with custom predicate
    // =========================================================================
    println!("--- Example 2: FilteredToolset with custom predicate ---");
    println!("  Only tools whose names start with 'get_'\n");

    let filtered_custom = FilteredToolset::with_name(
        weather_toolset.clone(),
        Box::new(|tool: &dyn Tool| tool.name().starts_with("get_")),
        "get_only_weather",
    );

    let tools = filtered_custom.tools(dummy_ctx.clone()).await?;
    println!("  Filtered tools: {:?}\n", tools.iter().map(|t| t.name()).collect::<Vec<_>>());

    // =========================================================================
    // Example 3: MergedToolset — combine multiple toolsets
    // =========================================================================
    println!("--- Example 3: MergedToolset ---");
    println!("  Merging weather + utility toolsets into one\n");

    let merged =
        MergedToolset::new("all_tools", vec![weather_toolset.clone(), utility_toolset.clone()]);

    let tools = merged.tools(dummy_ctx.clone()).await?;
    println!("  Merged tools: {:?}", tools.iter().map(|t| t.name()).collect::<Vec<_>>());

    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("merged_agent")
            .model(model.clone())
            .instruction(
                "You have weather tools and utility tools. \
                 Use the right tool for each question.",
            )
            .toolset(Arc::new(merged))
            .build()?,
    );

    let resp = run_agent(agent, "What's the weather in Paris, and what is 6*7?").await?;
    println!("  Response: {}\n", resp.chars().take(300).collect::<String>());

    // =========================================================================
    // Example 4: MergedToolset with builder pattern
    // =========================================================================
    println!("--- Example 4: MergedToolset builder ---");

    let merged_builder = MergedToolset::new("incremental", vec![weather_toolset.clone()])
        .with_toolset(utility_toolset.clone());

    let tools = merged_builder.tools(dummy_ctx.clone()).await?;
    println!("  Built incrementally: {:?}\n", tools.iter().map(|t| t.name()).collect::<Vec<_>>());

    // =========================================================================
    // Example 5: PrefixedToolset — namespace tool names
    // =========================================================================
    println!("--- Example 5: PrefixedToolset ---");
    println!("  Prefixing weather tools with 'wx_' to avoid name collisions\n");

    let prefixed = PrefixedToolset::new(weather_toolset.clone(), "wx");

    let tools = prefixed.tools(dummy_ctx.clone()).await?;
    println!("  Prefixed tools: {:?}", tools.iter().map(|t| t.name()).collect::<Vec<_>>());

    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("prefixed_agent")
            .model(model.clone())
            .instruction(
                "You have weather tools prefixed with 'wx_'. \
                 Use wx_get_weather for current weather and wx_get_forecast for forecasts.",
            )
            .toolset(Arc::new(prefixed))
            .build()?,
    );

    let resp = run_agent(agent, "What's the forecast for London?").await?;
    println!("  Response: {}\n", resp.chars().take(200).collect::<String>());

    // =========================================================================
    // Example 6: Composed — Prefix + Filter + Merge
    // =========================================================================
    println!("--- Example 6: Full Composition (Prefix + Filter + Merge) ---");
    println!("  Prefix weather tools, filter utilities to search+calc, merge both\n");

    let prefixed_weather = Arc::new(PrefixedToolset::new(weather_toolset.clone(), "wx"));

    let filtered_utils = Arc::new(FilteredToolset::new(
        utility_toolset.clone(),
        string_predicate(vec!["search_web".into(), "calculate".into()]),
    ));

    let composed = MergedToolset::new(
        "composed",
        vec![prefixed_weather as Arc<dyn Toolset>, filtered_utils as Arc<dyn Toolset>],
    );

    let tools = composed.tools(dummy_ctx.clone()).await?;
    println!("  Composed tools: {:?}", tools.iter().map(|t| t.name()).collect::<Vec<_>>());

    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("composed_agent")
            .model(model.clone())
            .instruction(
                "You have prefixed weather tools (wx_get_weather, wx_get_forecast), \
                 search_web, and calculate. Use the right tool for each question.",
            )
            .toolset(Arc::new(composed))
            .build()?,
    );

    let resp = run_agent(agent, "What's the weather in Berlin? Also, what is 123 * 456?").await?;
    println!("  Response: {}\n", resp.chars().take(300).collect::<String>());

    println!("=== Example Complete ===");
    Ok(())
}

// Minimal ReadonlyContext for toolset resolution outside an agent
struct DummyCtx;
#[async_trait]
impl ReadonlyContext for DummyCtx {
    fn invocation_id(&self) -> &str {
        "dummy"
    }
    fn agent_name(&self) -> &str {
        "dummy"
    }
    fn user_id(&self) -> &str {
        "user"
    }
    fn app_name(&self) -> &str {
        "compose"
    }
    fn session_id(&self) -> &str {
        "s1"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        // This is only used for toolset resolution, not for LLM calls
        static EMPTY: std::sync::LazyLock<Content> =
            std::sync::LazyLock::new(|| Content::new("user"));
        &EMPTY
    }
}
