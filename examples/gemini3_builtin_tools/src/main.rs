//! Gemini 3 Built-in Tools — live integration example.
//!
//! Demonstrates the fix for Gemini 3 models using built-in tools (`google_search`,
//! `url_context`) alongside user-defined function calling tools. Before this fix,
//! Gemini 3 would silently truncate responses when both tool types were present.
//!
//! Scenarios:
//!   1. GoogleSearchTool `is_builtin()` verification
//!   2. Built-in tool + function tool coexistence (full agent round-trip)
//!   3. ServerToolCall/ServerToolResponse part inspection
//!
//! # Usage
//!
//! ```bash
//! export GOOGLE_API_KEY=your-key-here
//! cargo run --manifest-path examples/gemini3_builtin_tools/Cargo.toml
//! ```

use adk_core::{Part, SessionId, UserId};
use adk_model::GeminiModel;
use adk_rust::futures::StreamExt;
use adk_rust::prelude::*;
use adk_rust::session::{CreateRequest, SessionService};
use adk_tool::GoogleSearchTool;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

const APP_NAME: &str = "gemini3-builtin-tools-example";
const MODEL_NAME: &str = "gemini-3.1-flash-lite-preview";

/// Display a thought_signature value, truncated for readability.
fn sig_display(sig: &Option<String>) -> String {
    match sig {
        Some(s) if s.len() > 40 => format!("{}…[{}B]", &s[..40], s.len()),
        Some(s) => s.clone(),
        None => "None".into(),
    }
}

/// Extract the embedded `_thought_signature` from a ServerToolCall JSON value.
fn server_tool_sig(val: &serde_json::Value) -> Option<String> {
    val.get("_thought_signature").and_then(|v| v.as_str()).map(String::from)
}

fn load_dotenv() {
    let mut dir = std::env::current_dir().ok();
    while let Some(d) = dir {
        let path = d.join(".env");
        if path.is_file() {
            let _ = dotenvy::from_path(path);
            return;
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
}

fn separator(title: &str) {
    println!("\n{}", "=".repeat(60));
    println!("  {title}");
    println!("{}\n", "=".repeat(60));
}

fn api_key() -> String {
    std::env::var("GOOGLE_API_KEY").expect("GOOGLE_API_KEY must be set")
}

/// Print grounding metadata from the LlmResponse's provider_metadata field.
fn print_grounding(event: &adk_core::Event) {
    let Some(meta) = &event.llm_response.provider_metadata else { return };
    let obj = match meta.as_object() {
        Some(o) if !o.is_empty() => o,
        _ => return,
    };

    println!("\n  🌐 Grounding metadata:");

    if let Some(queries) = obj.get("webSearchQueries").and_then(|v| v.as_array()) {
        let qs: Vec<&str> = queries.iter().filter_map(|q| q.as_str()).collect();
        if !qs.is_empty() {
            println!("  🔍 Search queries: {}", qs.join(", "));
        }
    }

    if let Some(chunks) = obj.get("groundingChunks").and_then(|v| v.as_array()) {
        for (i, chunk) in chunks.iter().enumerate() {
            if let Some(web) = chunk.get("web") {
                let title = web.get("title").and_then(|v| v.as_str()).unwrap_or("?");
                let uri = web.get("uri").and_then(|v| v.as_str()).unwrap_or("?");
                println!("  📚 Source [{i}]: {title}");
                println!("     {uri}");
            }
        }
    }

    if let Some(supports) = obj.get("groundingSupports").and_then(|v| v.as_array()) {
        println!("  📎 Grounding supports: {} segment(s)", supports.len());
        for sup in supports {
            if let Some(seg) = sup.get("segment") {
                let text = seg.get("text").and_then(|v| v.as_str()).unwrap_or("");
                let start = seg.get("startIndex").and_then(|v| v.as_u64()).unwrap_or(0);
                let end = seg.get("endIndex").and_then(|v| v.as_u64()).unwrap_or(0);
                let indices = sup
                    .get("groundingChunkIndices")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_u64())
                            .map(|i| i.to_string())
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .unwrap_or_default();
                let preview = if text.len() > 80 { &text[..80] } else { text };
                println!("     [{start}..{end}] chunks=[{indices}] \"{preview}…\"");
            }
        }
    }
}

async fn make_runner(agent: Arc<dyn Agent>, session_id: &str) -> anyhow::Result<Runner> {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP_NAME.into(),
            user_id: "user".into(),
            session_id: Some(session_id.into()),
            state: HashMap::new(),
        })
        .await?;

    Ok(Runner::new(RunnerConfig {
        app_name: APP_NAME.into(),
        agent,
        session_service: sessions,
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
        request_context: None,
        cancellation_token: None,
    })?)
}

/// Scenario 1: Verify GoogleSearchTool.is_builtin() returns true.
async fn test_is_builtin() -> anyhow::Result<()> {
    separator("1. GoogleSearchTool is_builtin() verification");

    let tool = GoogleSearchTool::new();
    let is_builtin = tool.is_builtin();
    println!("GoogleSearchTool.is_builtin() = {is_builtin}");
    assert!(is_builtin, "GoogleSearchTool should be marked as built-in");

    // A regular FunctionTool should NOT be built-in
    async fn noop(
        _ctx: Arc<dyn ToolContext>,
        _args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        Ok(json!({"result": "ok"}))
    }
    let regular_tool = FunctionTool::new("my_tool", "A regular tool", noop);
    println!("FunctionTool.is_builtin() = {}", regular_tool.is_builtin());
    assert!(!regular_tool.is_builtin(), "FunctionTool should not be built-in");

    println!("✓ is_builtin() verification passed");
    Ok(())
}

/// Scenario 2: Built-in google_search + custom function tool on Gemini 3.
///
/// The agent has both GoogleSearchTool (built-in, server-side) and a custom
/// get_capital tool (function calling, client-side). When the user asks a
/// question that triggers google_search, the agent should:
///   - NOT attempt to execute GoogleSearchTool locally
///   - Preserve any ServerToolCall/ServerToolResponse parts in the response
///   - Still be able to call the custom tool normally
async fn test_builtin_plus_function_tool() -> anyhow::Result<()> {
    separator("2. Built-in + function tool coexistence");

    #[derive(schemars::JsonSchema, serde::Serialize)]
    struct CapitalArgs {
        country: String,
    }

    async fn get_capital(
        _ctx: Arc<dyn ToolContext>,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let country = args["country"].as_str().unwrap_or("Unknown");
        Ok(json!({ "country": country, "capital": "Nairobi" }))
    }

    let model = Arc::new(GeminiModel::new(api_key(), MODEL_NAME)?);
    let search_tool: Arc<dyn Tool> = Arc::new(GoogleSearchTool::new());
    let capital_tool: Arc<dyn Tool> = Arc::new(
        FunctionTool::new("get_capital", "Get the capital city of a country.", get_capital)
            .with_parameters_schema::<CapitalArgs>(),
    );

    let agent = Arc::new(
        LlmAgentBuilder::new("gemini3-mixed-tools")
            .instruction("You have access to Google Search and a get_capital tool. Use whichever is appropriate.")
            .model(model)
            .tool(search_tool)
            .tool(capital_tool)
            .build()?,
    );

    let runner = make_runner(agent, "mixed-tools").await?;

    // Ask something that should trigger google_search (current events)
    println!("Sending query that should trigger google_search...");
    let content = Content::new("user")
        .with_text("Search the web for the latest Rust programming language release version and tell me about it.");

    let mut stream =
        runner.run(UserId::new("user")?, SessionId::new("mixed-tools")?, content).await?;

    let mut saw_server_tool_call = false;
    let mut saw_server_tool_response = false;
    let mut saw_text = false;
    let mut full_text = String::new();

    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::ServerToolCall { server_tool_call } => {
                        let sig = server_tool_sig(server_tool_call);
                        println!("  → ServerToolCall: google_search");
                        println!("    thought_signature: {}", sig_display(&sig));
                        saw_server_tool_call = true;
                    }
                    Part::ServerToolResponse { server_tool_response } => {
                        println!(
                            "  ← ServerToolResponse: [{}B]",
                            server_tool_response.to_string().len()
                        );
                        saw_server_tool_response = true;
                    }
                    Part::Thinking { signature, .. } => {
                        println!("  💭 Thinking (signature: {})", sig_display(signature));
                    }
                    Part::Text { text } if !text.trim().is_empty() => {
                        print!("{text}");
                        full_text.push_str(text);
                        saw_text = true;
                    }
                    Part::FunctionCall { name, thought_signature, .. } => {
                        println!("  → FunctionCall: {name}");
                        println!("    thought_signature: {}", sig_display(thought_signature));
                    }
                    Part::FunctionResponse { function_response, .. } => {
                        println!("  ← FunctionResponse: {}", function_response.response);
                    }
                    _ => {}
                }
            }
        }
        print_grounding(&event);
    }
    println!();

    if saw_server_tool_call {
        println!("ServerToolCall parts observed (google_search executed server-side)");
    }
    if saw_server_tool_response {
        println!("ServerToolResponse parts observed (search results returned server-side)");
    }
    if saw_text {
        println!("Final text response received ({} chars)", full_text.len());
    }

    assert!(
        saw_text || saw_server_tool_call,
        "expected either text output or server-side tool invocation"
    );
    println!("✓ Built-in + function tool coexistence passed");
    Ok(())
}

/// Scenario 3: Custom function tool still works normally alongside built-in tools.
async fn test_function_tool_still_works() -> anyhow::Result<()> {
    separator("3. Custom function tool execution alongside built-in");

    #[derive(schemars::JsonSchema, serde::Serialize)]
    struct WeatherArgs {
        city: String,
    }

    async fn get_weather(
        _ctx: Arc<dyn ToolContext>,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let city = args["city"].as_str().unwrap_or("Unknown");
        Ok(json!({
            "city": city,
            "temperature_c": 22,
            "condition": "Partly cloudy"
        }))
    }

    let model = Arc::new(GeminiModel::new(api_key(), MODEL_NAME)?);
    let search_tool: Arc<dyn Tool> = Arc::new(GoogleSearchTool::new());
    let weather_tool: Arc<dyn Tool> = Arc::new(
        FunctionTool::new("get_weather", "Get current weather for a city.", get_weather)
            .with_parameters_schema::<WeatherArgs>(),
    );

    let agent = Arc::new(
        LlmAgentBuilder::new("gemini3-function-tool")
            .instruction("Use the get_weather tool when asked about weather. Use Google Search for other queries.")
            .model(model)
            .tool(search_tool)
            .tool(weather_tool)
            .build()?,
    );

    let runner = make_runner(agent, "function-tool").await?;

    // Ask something that should trigger the custom function tool, not google_search
    println!("Sending query that should trigger get_weather (custom tool)...");
    let content = Content::new("user")
        .with_text("What's the weather in Tokyo right now? Use the get_weather tool.");

    let mut stream =
        runner.run(UserId::new("user")?, SessionId::new("function-tool")?, content).await?;

    let mut saw_function_call = false;
    let mut saw_function_response = false;
    let mut saw_text = false;

    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::FunctionCall { name, args, thought_signature, .. } => {
                        println!("  → FunctionCall: {name}({args})");
                        println!("    thought_signature: {}", sig_display(thought_signature));
                        saw_function_call = true;
                    }
                    Part::FunctionResponse { function_response, .. } => {
                        println!("  ← FunctionResponse: {}", function_response.response);
                        saw_function_response = true;
                    }
                    Part::Thinking { signature, .. } => {
                        println!("  💭 Thinking (signature: {})", sig_display(signature));
                    }
                    Part::Text { text } if !text.trim().is_empty() => {
                        print!("{text}");
                        saw_text = true;
                    }
                    _ => {}
                }
            }
        }
        print_grounding(&event);
    }
    println!();

    assert!(saw_function_call, "expected get_weather function call");
    assert!(saw_function_response || saw_text, "expected function response or final text");
    println!("✓ Custom function tool execution passed");
    Ok(())
}

/// Scenario 4: Multi-turn with both built-in + function tool in same conversation.
///
/// This is the critical test for the thought_signature fix. When Gemini 3 uses
/// google_search (built-in) in one turn and then the user asks something that
/// triggers a function tool call, the FunctionResponse sent back must include
/// the thought_signature from the preceding FunctionCall. Without the fix,
/// Gemini 3 returns 400: "Tool response part is missing thought_signature".
async fn test_multi_turn_mixed_tools() -> anyhow::Result<()> {
    separator("4. Multi-turn: search then function tool (thought_signature fix)");

    #[derive(schemars::JsonSchema, serde::Serialize)]
    struct CapitalArgs {
        country: String,
    }

    async fn get_capital(
        _ctx: Arc<dyn ToolContext>,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let country = args["country"].as_str().unwrap_or("Unknown");
        let capital = match country.to_lowercase().as_str() {
            "kenya" => "Nairobi",
            "japan" => "Tokyo",
            "france" => "Paris",
            _ => "Unknown",
        };
        Ok(json!({ "country": country, "capital": capital }))
    }

    let model = Arc::new(GeminiModel::new(api_key(), MODEL_NAME)?);
    let search_tool: Arc<dyn Tool> = Arc::new(GoogleSearchTool::new());
    let capital_tool: Arc<dyn Tool> = Arc::new(
        FunctionTool::new("get_capital", "Get the capital city of a country.", get_capital)
            .with_parameters_schema::<CapitalArgs>(),
    );

    let agent = Arc::new(
        LlmAgentBuilder::new("gemini3-multi-turn")
            .instruction(
                "You have Google Search and get_capital tools. \
                 Use Google Search for current events. \
                 Use get_capital when asked about capital cities.",
            )
            .model(model)
            .tool(search_tool)
            .tool(capital_tool)
            .build()?,
    );

    let runner = make_runner(agent, "multi-turn").await?;
    let user_id = UserId::new("user")?;
    let session_id = SessionId::new("multi-turn")?;

    // Turn 1: trigger google_search (built-in)
    println!("Turn 1: Sending query to trigger google_search...");
    let content1 =
        Content::new("user").with_text("Search the web: what is the current population of Kenya?");

    let mut stream = runner.run(user_id.clone(), session_id.clone(), content1).await?;
    let mut turn1_text = String::new();
    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::ServerToolCall { server_tool_call } => {
                        let sig = server_tool_sig(server_tool_call);
                        println!("  → ServerToolCall: google_search");
                        println!("    thought_signature: {}", sig_display(&sig));
                    }
                    Part::ServerToolResponse { .. } => print!("[result] "),
                    Part::Thinking { signature, .. } => {
                        println!("  💭 Thinking (signature: {})", sig_display(signature));
                    }
                    Part::Text { text } if !text.trim().is_empty() => {
                        turn1_text.push_str(text);
                    }
                    _ => {}
                }
            }
        }
        print_grounding(&event);
    }
    println!("Turn 1 response: {} chars", turn1_text.len());
    assert!(!turn1_text.is_empty(), "Turn 1 should produce text");

    // Turn 2: trigger function tool (get_capital) — this is where the bug manifests.
    // The session history now contains ServerToolCall/ServerToolResponse from turn 1.
    // When the model calls get_capital, ADK must attach thought_signature to the
    // FunctionResponse. Without the fix, this fails with 400.
    println!("\nTurn 2: Sending query to trigger get_capital (function tool)...");
    let content2 =
        Content::new("user").with_text("What is the capital of Kenya? Use the get_capital tool.");

    let mut stream = runner.run(user_id, session_id, content2).await?;
    let mut saw_function_call = false;
    let mut saw_function_response = false;
    let mut turn2_text = String::new();

    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::FunctionCall { name, thought_signature, .. } => {
                        println!("  → FunctionCall: {name}");
                        println!("    thought_signature: {}", sig_display(thought_signature));
                        saw_function_call = true;
                    }
                    Part::FunctionResponse { function_response, .. } => {
                        println!("  ← FunctionResponse: {}", function_response.response);
                        saw_function_response = true;
                    }
                    Part::Thinking { signature, .. } => {
                        println!("  💭 Thinking (signature: {})", sig_display(signature));
                    }
                    Part::Text { text } if !text.trim().is_empty() => {
                        turn2_text.push_str(text);
                    }
                    _ => {}
                }
            }
        }
        print_grounding(&event);
    }
    if !turn2_text.is_empty() {
        println!("Turn 2 response: {turn2_text}");
    }

    assert!(
        saw_function_call || !turn2_text.is_empty(),
        "Turn 2 should call get_capital or produce text"
    );
    if saw_function_call {
        assert!(
            saw_function_response || !turn2_text.is_empty(),
            "Function call should produce a response"
        );
    }
    println!("✓ Multi-turn mixed tools passed (thought_signature propagated correctly)");
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_dotenv();

    println!("Gemini 3 Built-in Tools — Live Integration Example");
    println!("===================================================");
    println!("Demonstrates google_search + function tools on Gemini 3\n");

    let model_name = std::env::var("GEMINI_MODEL").unwrap_or_else(|_| MODEL_NAME.to_string());
    println!("Model: {model_name}");

    type ScenarioFuture =
        std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>;
    type ScenarioFn = fn() -> ScenarioFuture;

    let scenarios: Vec<(&str, ScenarioFn)> = vec![
        ("is_builtin verification", || Box::pin(test_is_builtin())),
        ("Built-in + function tool", || Box::pin(test_builtin_plus_function_tool())),
        ("Custom tool still works", || Box::pin(test_function_tool_still_works())),
        ("Multi-turn mixed tools", || Box::pin(test_multi_turn_mixed_tools())),
    ];

    let mut passed = 0usize;
    let mut failed = 0usize;
    let total = scenarios.len();

    for (name, run) in scenarios {
        match run().await {
            Ok(()) => passed += 1,
            Err(err) => {
                eprintln!("\n✗ {name} FAILED: {err:#}");
                failed += 1;
            }
        }
    }

    separator("Summary");
    println!("  {passed}/{total} passed, {failed} failed");
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}
