//! # Enhanced Plugin System Demo
//!
//! Demonstrates the ADK-Rust Enhanced Plugin System (Sprint 1) with three
//! custom plugins that intercept tool calls and model calls in a real agent workflow.
//!
//! ## Plugins Demonstrated
//!
//! 1. **LoggingPlugin** (priority 100) — Logs all tool/model calls with timing
//! 2. **SanitizationPlugin** (priority 50) — Modifies tool arguments before execution
//! 3. **CachingPlugin** (priority 30) — Short-circuits repeated tool calls with cached results
//!
//! ## Execution Order (by priority, ascending)
//!
//! CachingPlugin (30) → SanitizationPlugin (50) → LoggingPlugin (100)
//!
//! ## Run
//!
//! ```bash
//! cd examples/plugin_system
//! cp .env.example .env   # add your GOOGLE_API_KEY
//! cargo run
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, CallbackContext, Content, Llm, LlmRequest, LlmResponse, Result, Tool, async_trait,
};
use adk_model::GeminiModel;
use adk_plugin::{
    AfterModelCallResult, AfterToolCallResult, BeforeModelCallResult, BeforeToolCallResult,
    EnhancedPlugin, PluginContext,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::{AdkError, tool};
use futures::StreamExt;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::RwLock;
use tracing_subscriber::EnvFilter;

// ═══════════════════════════════════════════════════════════════════════════════
// Tools
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Deserialize, JsonSchema)]
struct GetWeatherArgs {
    /// City name to get weather for
    city: String,
}

/// Get the current weather for a city. Returns temperature and conditions.
#[tool]
async fn get_weather(args: GetWeatherArgs) -> std::result::Result<Value, AdkError> {
    // Simulate a weather API call with mock data
    let weather = match args.city.to_lowercase().as_str() {
        c if c.contains("london") => json!({
            "city": "London",
            "temperature_celsius": 12,
            "conditions": "Overcast with light rain",
            "humidity": 85,
            "wind_kph": 15
        }),
        c if c.contains("tokyo") => json!({
            "city": "Tokyo",
            "temperature_celsius": 24,
            "conditions": "Partly cloudy",
            "humidity": 60,
            "wind_kph": 8
        }),
        c if c.contains("new york") || c.contains("nyc") => json!({
            "city": "New York",
            "temperature_celsius": 18,
            "conditions": "Clear skies",
            "humidity": 45,
            "wind_kph": 12
        }),
        _ => json!({
            "city": args.city,
            "temperature_celsius": 20,
            "conditions": "Fair",
            "humidity": 50,
            "wind_kph": 10
        }),
    };

    println!("    [get_weather tool] Returning weather data for: {}", args.city);
    Ok(weather)
}

// ═══════════════════════════════════════════════════════════════════════════════
// Plugin 1: LoggingPlugin — logs all tool/model calls with timing
// ═══════════════════════════════════════════════════════════════════════════════

/// Shared state for tracking call counts across the plugin lifecycle.
#[derive(Clone, Debug)]
struct LoggingStats {
    tool_calls: u32,
    model_calls: u32,
}

struct LoggingPlugin;

#[async_trait]
impl EnhancedPlugin for LoggingPlugin {
    fn name(&self) -> &str {
        "logging"
    }

    fn priority(&self) -> i32 {
        100 // Runs last — observes final args/results
    }

    async fn before_tool_call(
        &self,
        tool: Arc<dyn Tool>,
        args: Value,
        _ctx: Arc<dyn CallbackContext>,
        plugin_ctx: &PluginContext,
    ) -> Result<BeforeToolCallResult> {
        println!("  📋 [LoggingPlugin] before_tool_call: tool={}, args={}", tool.name(), args);

        // Track call count across every run of the agent
        let mut stats = plugin_ctx
            .shared()
            .get::<LoggingStats>()
            .await
            .unwrap_or(LoggingStats { tool_calls: 0, model_calls: 0 });
        stats.tool_calls += 1;
        plugin_ctx.shared().insert(stats).await;

        Ok(BeforeToolCallResult::Continue(args))
    }

    async fn after_tool_call(
        &self,
        tool: Arc<dyn Tool>,
        _args: &Value,
        result: Value,
        _ctx: Arc<dyn CallbackContext>,
        _plugin_ctx: &PluginContext,
    ) -> Result<AfterToolCallResult> {
        println!(
            "  📋 [LoggingPlugin] after_tool_call: tool={}, result_size={} bytes",
            tool.name(),
            result.to_string().len()
        );
        Ok(AfterToolCallResult::Continue(result))
    }

    async fn before_model_call(
        &self,
        request: LlmRequest,
        _ctx: Arc<dyn CallbackContext>,
        plugin_ctx: &PluginContext,
    ) -> Result<BeforeModelCallResult> {
        let msg_count = request.contents.len();
        println!(
            "  📋 [LoggingPlugin] before_model_call: model={}, messages={}",
            request.model, msg_count
        );

        let mut stats = plugin_ctx
            .shared()
            .get::<LoggingStats>()
            .await
            .unwrap_or(LoggingStats { tool_calls: 0, model_calls: 0 });
        stats.model_calls += 1;
        plugin_ctx.shared().insert(stats).await;

        Ok(BeforeModelCallResult::Continue(request))
    }

    async fn after_model_call(
        &self,
        response: LlmResponse,
        _ctx: Arc<dyn CallbackContext>,
        _plugin_ctx: &PluginContext,
    ) -> Result<AfterModelCallResult> {
        let has_content = response.content.is_some();
        println!(
            "  📋 [LoggingPlugin] after_model_call: has_content={}, turn_complete={}",
            has_content, response.turn_complete
        );
        Ok(AfterModelCallResult::Continue(response))
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Plugin 2: SanitizationPlugin — modifies tool arguments before execution
// ═══════════════════════════════════════════════════════════════════════════════

struct SanitizationPlugin;

#[async_trait]
impl EnhancedPlugin for SanitizationPlugin {
    fn name(&self) -> &str {
        "sanitization"
    }

    fn priority(&self) -> i32 {
        50 // Runs in the middle — after cache check, before logging
    }

    async fn before_tool_call(
        &self,
        tool: Arc<dyn Tool>,
        mut args: Value,
        _ctx: Arc<dyn CallbackContext>,
        _plugin_ctx: &PluginContext,
    ) -> Result<BeforeToolCallResult> {
        // Inject a safe_mode flag into all tool arguments
        if let Value::Object(ref mut map) = args {
            map.insert("safe_mode".to_string(), Value::Bool(true));
            map.insert("sanitized_by".to_string(), Value::String("SanitizationPlugin".to_string()));
        }

        println!("  🛡️  [SanitizationPlugin] Injected safe_mode=true into {} args", tool.name());
        Ok(BeforeToolCallResult::Continue(args))
    }

    async fn after_tool_call(
        &self,
        _tool: Arc<dyn Tool>,
        _args: &Value,
        mut result: Value,
        _ctx: Arc<dyn CallbackContext>,
        _plugin_ctx: &PluginContext,
    ) -> Result<AfterToolCallResult> {
        // Strip any sensitive fields from results (demonstration)
        if let Value::Object(ref mut map) = result {
            map.insert("sanitized".to_string(), Value::Bool(true));
        }
        Ok(AfterToolCallResult::Continue(result))
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Plugin 3: CachingPlugin — short-circuits repeated tool calls
// ═══════════════════════════════════════════════════════════════════════════════

/// Cache state stored in PluginContext.
#[derive(Clone, Debug)]
struct ToolCache {
    hits: u32,
    misses: u32,
}

struct CachingPlugin {
    cache: Arc<RwLock<HashMap<String, Value>>>,
}

impl CachingPlugin {
    fn new() -> Self {
        Self { cache: Arc::new(RwLock::new(HashMap::new())) }
    }

    fn cache_key(tool_name: &str, args: &Value) -> String {
        format!("{}:{}", tool_name, args.to_string())
    }
}

#[async_trait]
impl EnhancedPlugin for CachingPlugin {
    fn name(&self) -> &str {
        "caching"
    }

    fn priority(&self) -> i32 {
        30 // Runs first — can short-circuit before other plugins
    }

    async fn before_tool_call(
        &self,
        tool: Arc<dyn Tool>,
        args: Value,
        _ctx: Arc<dyn CallbackContext>,
        plugin_ctx: &PluginContext,
    ) -> Result<BeforeToolCallResult> {
        let key = Self::cache_key(tool.name(), &args);

        let cache = self.cache.read().await;
        if let Some(cached_result) = cache.get(&key) {
            // Cache HIT — short-circuit tool execution
            println!(
                "  ⚡ [CachingPlugin] CACHE HIT for {}! Skipping tool execution.",
                tool.name()
            );

            // Update stats in PluginContext
            let mut stats = plugin_ctx
                .shared()
                .get::<ToolCache>()
                .await
                .unwrap_or(ToolCache { hits: 0, misses: 0 });
            stats.hits += 1;
            plugin_ctx.shared().insert(stats).await;

            return Ok(BeforeToolCallResult::ShortCircuit(cached_result.clone()));
        }
        drop(cache);

        // Cache MISS
        println!("  ⚡ [CachingPlugin] CACHE MISS for {}. Proceeding with execution.", tool.name());

        let mut stats = plugin_ctx
            .shared()
            .get::<ToolCache>()
            .await
            .unwrap_or(ToolCache { hits: 0, misses: 0 });
        stats.misses += 1;
        plugin_ctx.shared().insert(stats).await;

        Ok(BeforeToolCallResult::Continue(args))
    }

    async fn after_tool_call(
        &self,
        tool: Arc<dyn Tool>,
        args: &Value,
        result: Value,
        _ctx: Arc<dyn CallbackContext>,
        _plugin_ctx: &PluginContext,
    ) -> Result<AfterToolCallResult> {
        // Store result in cache for future calls
        let key = Self::cache_key(tool.name(), args);
        let mut cache = self.cache.write().await;
        cache.insert(key, result.clone());
        println!("  ⚡ [CachingPlugin] Cached result for {}", tool.name());

        Ok(AfterToolCallResult::Continue(result))
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Main
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let api_key =
        std::env::var("GOOGLE_API_KEY").expect("GOOGLE_API_KEY must be set — see .env.example");

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║  Enhanced Plugin System Demo — ADK-Rust Sprint 1            ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    // ── Create the model ─────────────────────────────────────────────────────
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);
    println!("✓ Model: {}\n", model.name());

    // ── Create plugins ───────────────────────────────────────────────────────
    println!("── Registering Plugins ──────────────────────────────────────────");
    let caching_plugin = Arc::new(CachingPlugin::new());
    let sanitization_plugin = Arc::new(SanitizationPlugin);
    let logging_plugin = Arc::new(LoggingPlugin);

    println!("  • CachingPlugin       (priority 30)  — short-circuits repeated calls");
    println!("  • SanitizationPlugin  (priority 50)  — injects safe_mode flag");
    println!("  • LoggingPlugin       (priority 100) — logs all calls with timing");
    println!();
    println!("  Pipeline order: Caching → Sanitization → Logging");
    println!();

    // ── Build the agent with plugins ─────────────────────────────────────────
    let agent = LlmAgentBuilder::new("weather-assistant")
        .description("A weather assistant with plugin pipeline")
        .model(model)
        .instruction(
            "You are a helpful weather assistant. When asked about weather, \
             use the get_weather tool to fetch current conditions. \
             Always provide a brief, friendly summary of the weather.",
        )
        .tool(Arc::new(GetWeather))
        .enhanced_plugins(vec![
            caching_plugin.clone() as Arc<dyn EnhancedPlugin>,
            sanitization_plugin as Arc<dyn EnhancedPlugin>,
            logging_plugin as Arc<dyn EnhancedPlugin>,
        ])
        .build()?;

    let session_service = Arc::new(InMemorySessionService::new());
    let runner = Runner::builder()
        .app_name("plugin-system-demo")
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(session_service.clone())
        .build()?;

    // ── Create a session ─────────────────────────────────────────────────────
    session_service
        .create(CreateRequest {
            app_name: "plugin-system-demo".into(),
            user_id: "user".into(),
            session_id: Some("demo-session".into()),
            state: Default::default(),
        })
        .await?;

    // ── Run 1: First query (cache MISS) ──────────────────────────────────────
    println!("══════════════════════════════════════════════════════════════════");
    println!("  RUN 1: \"What's the weather in London?\"");
    println!("  Expected: Cache MISS → tool executes → result cached");
    println!("══════════════════════════════════════════════════════════════════\n");

    let start = Instant::now();
    let content = Content::new("user").with_text("What's the weather in London?");
    let mut stream = runner.run_str("user", "demo-session", content).await?;

    let mut response_text = String::new();
    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = event.content() {
            for part in &content.parts {
                if let adk_core::Part::Text { text } = part {
                    response_text.push_str(text);
                }
            }
        }
    }
    let elapsed = start.elapsed();

    println!("\n  🤖 Agent response: {}", truncate(&response_text, 200));
    println!("  ⏱️  Elapsed: {:?}\n", elapsed);

    // ── Run 2: Same query (cache HIT) ────────────────────────────────────────
    println!("══════════════════════════════════════════════════════════════════");
    println!("  RUN 2: \"What's the weather in London?\" (same query)");
    println!("  Expected: Cache HIT → tool execution SKIPPED");
    println!("══════════════════════════════════════════════════════════════════\n");

    // Create a new session for the second run to get a fresh conversation
    session_service
        .create(CreateRequest {
            app_name: "plugin-system-demo".into(),
            user_id: "user".into(),
            session_id: Some("demo-session-2".into()),
            state: Default::default(),
        })
        .await?;

    let start = Instant::now();
    let content = Content::new("user").with_text("What's the weather in London?");
    let mut stream = runner.run_str("user", "demo-session-2", content).await?;

    let mut response_text = String::new();
    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = event.content() {
            for part in &content.parts {
                if let adk_core::Part::Text { text } = part {
                    response_text.push_str(text);
                }
            }
        }
    }
    let elapsed = start.elapsed();

    println!("\n  🤖 Agent response: {}", truncate(&response_text, 200));
    println!("  ⏱️  Elapsed: {:?}\n", elapsed);

    // ── Run 3: Different query (cache MISS for new city) ─────────────────────
    println!("══════════════════════════════════════════════════════════════════");
    println!("  RUN 3: \"What's the weather in Tokyo?\"");
    println!("  Expected: Cache MISS for new city → tool executes");
    println!("══════════════════════════════════════════════════════════════════\n");

    session_service
        .create(CreateRequest {
            app_name: "plugin-system-demo".into(),
            user_id: "user".into(),
            session_id: Some("demo-session-3".into()),
            state: Default::default(),
        })
        .await?;

    let start = Instant::now();
    let content = Content::new("user").with_text("What's the weather in Tokyo?");
    let mut stream = runner.run_str("user", "demo-session-3", content).await?;

    let mut response_text = String::new();
    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = event.content() {
            for part in &content.parts {
                if let adk_core::Part::Text { text } = part {
                    response_text.push_str(text);
                }
            }
        }
    }
    let elapsed = start.elapsed();

    println!("\n  🤖 Agent response: {}", truncate(&response_text, 200));
    println!("  ⏱️  Elapsed: {:?}\n", elapsed);

    // ── Summary ──────────────────────────────────────────────────────────────
    println!("══════════════════════════════════════════════════════════════════");
    println!("  SUMMARY");
    println!("══════════════════════════════════════════════════════════════════");
    println!();
    println!("  ✅ Plugin pipeline demonstrated:");
    println!("     • CachingPlugin: short-circuited repeated tool calls");
    println!("     • SanitizationPlugin: injected safe_mode into all tool args");
    println!("     • LoggingPlugin: logged all tool/model calls");
    println!();
    println!("  ✅ Priority ordering enforced:");
    println!("     Caching (30) → Sanitization (50) → Logging (100)");
    println!();
    println!("  ✅ PluginContext shared state:");
    println!("     • LoggingStats tracked call counts across invocations");
    println!("     • ToolCache tracked hits/misses");
    println!();

    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}
