//! Gemini native tools — live agentic example matrix.
//!
//! Demonstrates every Gemini native tool wrapper currently exposed by
//! ADK-Rust. The examples stay agentic: built-in tools coexist with function
//! tools, grounding metadata is surfaced in the agent stream, code execution is
//! captured as native protocol parts, and the final scenario preserves the
//! multi-turn thought-signature fix.
//!
//! Scenarios:
//!   1. Google Search + function tool coexistence
//!   2. URL context + function tool coexistence
//!   3. Google Maps + function tool coexistence
//!   4. Code execution + function tool coexistence
//!   5. File search + function tool coexistence
//!   6. Computer use invocation surfaced in the agent stream
//!   7. Multi-turn: native tool first, function tool second
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
use adk_tool::{
    GeminiCodeExecutionTool, GeminiComputerEnvironment, GeminiComputerUseTool,
    GeminiFileSearchTool, GoogleMapsContext, GoogleMapsTool, GoogleSearchTool, UrlContextTool,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

const APP_NAME: &str = "gemini3-builtin-tools-example";
const MODEL_NAME: &str = "gemini-3-pro-preview";
const MAPS_MODEL_NAME: &str = "gemini-2.5-flash";
const COMPUTER_USE_MODEL_NAME: &str = "gemini-2.5-computer-use-preview-10-2025";

#[derive(Debug)]
enum ScenarioOutcome {
    Passed,
    Skipped(String),
}

#[derive(Debug, Default)]
struct StreamSummary {
    saw_text: bool,
    saw_function_call: bool,
    saw_function_response: bool,
    saw_provider_metadata: bool,
    server_tool_calls: Vec<String>,
    server_tool_responses: Vec<String>,
    text: String,
}

#[derive(schemars::JsonSchema, serde::Serialize)]
struct ToolStatusArgs {
    tool_name: String,
    note: String,
}

type ScenarioFuture = Pin<Box<dyn Future<Output = anyhow::Result<ScenarioOutcome>> + Send>>;
type ScenarioFn = fn() -> ScenarioFuture;

fn sig_display(sig: &Option<String>) -> String {
    match sig {
        Some(s) if s.len() > 40 => format!("{}…[{}B]", &s[..40], s.len()),
        Some(s) => s.clone(),
        None => "None".into(),
    }
}

fn gemini_server_tool_kind(value: &Value) -> String {
    if value.get("toolCall").is_some() {
        "tool_call".to_string()
    } else if value.get("toolResponse").is_some() {
        "tool_response".to_string()
    } else if value.get("executableCode").is_some() {
        "executable_code".to_string()
    } else if value.get("codeExecutionResult").is_some() {
        "code_execution_result".to_string()
    } else {
        "unknown".to_string()
    }
}

fn server_tool_sig(val: &serde_json::Value) -> Option<String> {
    val.get("thoughtSignature")
        .and_then(|v| v.as_str())
        .or_else(|| {
            val.get("toolCall")
                .and_then(|tool_call| tool_call.get("_thought_signature"))
                .and_then(|v| v.as_str())
        })
        .map(String::from)
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

fn scenario_model(env_name: &str, fallback: &str) -> String {
    optional_env(env_name)
        .or_else(|| optional_env("GEMINI_MODEL"))
        .unwrap_or_else(|| fallback.to_string())
}

fn default_model_name() -> String {
    scenario_model("GEMINI_MODEL", MODEL_NAME)
}

fn skip_known_gemini_incompatibility(
    error: &impl std::fmt::Display,
    tool_name: &str,
    model: &str,
) -> Option<String> {
    let message = error.to_string();
    if message.contains("is not enabled for")
        || message.contains("not found for API version")
        || message.contains("404")
        || message.contains("FAILED_PRECONDITION")
    {
        return Some(format!(
            "{tool_name} is not available for model '{model}' or this API key"
        ));
    }
    None
}

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.trim().is_empty())
}

fn csv_env(name: &str) -> Vec<String> {
    optional_env(name)
        .into_iter()
        .flat_map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn saw_kind(summary: &StreamSummary, needle: &str) -> bool {
    summary.server_tool_calls.iter().any(|kind| kind.contains(needle))
        || summary.server_tool_responses.iter().any(|kind| kind.contains(needle))
}

fn skipped(message: impl Into<String>) -> anyhow::Result<ScenarioOutcome> {
    let message = message.into();
    println!("↷ skipped: {message}");
    Ok(ScenarioOutcome::Skipped(message))
}

fn passed() -> ScenarioOutcome {
    ScenarioOutcome::Passed
}

async fn record_tool_status(
    _ctx: Arc<dyn ToolContext>,
    args: serde_json::Value,
) -> Result<serde_json::Value> {
    Ok(json!({
        "acknowledged": true,
        "tool_name": args["tool_name"].as_str().unwrap_or("unknown"),
        "note": args["note"].as_str().unwrap_or(""),
    }))
}

fn note_tool() -> Arc<dyn Tool> {
    Arc::new(
        FunctionTool::new(
            "record_tool_status",
            "Records which Gemini native tool the agent used and why.",
            record_tool_status,
        )
        .with_parameters_schema::<ToolStatusArgs>(),
    )
}

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
        for (index, chunk) in chunks.iter().enumerate() {
            if let Some(web) = chunk.get("web") {
                let title = web.get("title").and_then(|v| v.as_str()).unwrap_or("?");
                let uri = web.get("uri").and_then(|v| v.as_str()).unwrap_or("?");
                println!("  📚 Source [{index}]: {title}");
                println!("     {uri}");
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

async fn run_agent_turn(
    agent_name: &str,
    model_name: &str,
    session_id: &str,
    instruction: &str,
    tools: Vec<Arc<dyn Tool>>,
    prompt: &str,
) -> anyhow::Result<StreamSummary> {
    let model = Arc::new(GeminiModel::new(api_key(), model_name)?);

    let mut builder = LlmAgentBuilder::new(agent_name).instruction(instruction).model(model);
    for tool in tools {
        builder = builder.tool(tool);
    }
    let agent = Arc::new(builder.build()?);
    let runner = make_runner(agent, session_id).await?;

    let mut stream = runner
        .run(
            UserId::new("user")?,
            SessionId::new(session_id)?,
            Content::new("user").with_text(prompt),
        )
        .await?;

    let mut summary = StreamSummary::default();
    while let Some(event) = stream.next().await {
        let event = event?;
        if event.llm_response.provider_metadata.is_some() {
            summary.saw_provider_metadata = true;
            print_grounding(&event);
        }

        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::ServerToolCall { server_tool_call } => {
                        let kind = gemini_server_tool_kind(server_tool_call);
                        println!("  → ServerToolCall: {kind}");
                        println!(
                            "    thought_signature: {}",
                            sig_display(&server_tool_sig(server_tool_call))
                        );
                        summary.server_tool_calls.push(kind);
                    }
                    Part::ServerToolResponse { server_tool_response } => {
                        let kind = gemini_server_tool_kind(server_tool_response);
                        println!(
                            "  ← ServerToolResponse: {kind} [{}B]",
                            server_tool_response.to_string().len()
                        );
                        summary.server_tool_responses.push(kind);
                    }
                    Part::FunctionCall { name, args, thought_signature, .. } => {
                        println!("  → FunctionCall: {name}({args})");
                        println!("    thought_signature: {}", sig_display(thought_signature));
                        summary.saw_function_call = true;
                    }
                    Part::FunctionResponse { function_response, .. } => {
                        println!("  ← FunctionResponse: {}", function_response.response);
                        summary.saw_function_response = true;
                    }
                    Part::Thinking { signature, .. } => {
                        println!("  💭 Thinking (signature: {})", sig_display(signature));
                    }
                    Part::Text { text } if !text.trim().is_empty() => {
                        print!("{text}");
                        summary.text.push_str(text);
                        summary.saw_text = true;
                    }
                    _ => {}
                }
            }
        }
    }
    if summary.saw_text {
        println!();
    }

    Ok(summary)
}

async fn test_google_search_plus_function_tool() -> anyhow::Result<ScenarioOutcome> {
    separator("1. Google Search + function tool coexistence");
    let model = scenario_model("GEMINI_GOOGLE_SEARCH_MODEL", MODEL_NAME);

    let summary = match run_agent_turn(
        "gemini-google-search",
        &model,
        "gemini-google-search",
        "You are a research agent. Use Google Search for current information, then call record_tool_status.",
        vec![Arc::new(GoogleSearchTool::new()), note_tool()],
        "Use Google Search to find the latest stable Rust release. After that, call record_tool_status with tool_name 'google_search' and a short note.",
    )
    .await
    {
        Ok(summary) => summary,
        Err(error) => {
            if let Some(reason) =
                skip_known_gemini_incompatibility(&error, "google_search", &model)
            {
                return skipped(reason);
            }
            return Err(error);
        }
    };

    assert!(
        saw_kind(&summary, "tool_call") || summary.saw_provider_metadata || summary.saw_text,
        "expected grounded output from Google Search"
    );
    Ok(passed())
}

async fn test_url_context_plus_function_tool() -> anyhow::Result<ScenarioOutcome> {
    separator("2. URL context + function tool coexistence");
    let model = scenario_model("GEMINI_URL_CONTEXT_MODEL", MODEL_NAME);

    let summary = match run_agent_turn(
        "gemini-url-context",
        &model,
        "gemini-url-context",
        "You can use URL context to read web pages. After using it, call record_tool_status.",
        vec![Arc::new(UrlContextTool::new()), note_tool()],
        "Use URL context to summarize https://www.rust-lang.org/learn in two sentences. After that, call record_tool_status with tool_name 'url_context'.",
    )
    .await
    {
        Ok(summary) => summary,
        Err(error) => {
            if let Some(reason) =
                skip_known_gemini_incompatibility(&error, "url_context", &model)
            {
                return skipped(reason);
            }
            return Err(error);
        }
    };

    assert!(summary.saw_text, "expected text output from URL context");
    Ok(passed())
}

async fn test_google_maps_plus_function_tool() -> anyhow::Result<ScenarioOutcome> {
    separator("3. Google Maps + function tool coexistence");
    let model = scenario_model("GEMINI_GOOGLE_MAPS_MODEL", MAPS_MODEL_NAME);

    let summary = match run_agent_turn(
        "gemini-google-maps",
        &model,
        "gemini-google-maps",
        "You can use Google Maps grounding for local recommendations. After using it, call record_tool_status.",
        vec![
            Arc::new(
                GoogleMapsTool::new()
                    .with_widget(true)
                    .with_context(GoogleMapsContext::new(40.7580, -73.9855)),
            ),
            note_tool(),
        ],
        "Use Google Maps to suggest two coffee shops near Times Square in New York City. After that, call record_tool_status with tool_name 'google_maps'.",
    )
    .await
    {
        Ok(summary) => summary,
        Err(error) => {
            if let Some(reason) =
                skip_known_gemini_incompatibility(&error, "google_maps", &model)
            {
                return skipped(reason);
            }
            return Err(error);
        }
    };

    assert!(
        summary.saw_text || summary.saw_provider_metadata,
        "expected text or grounding metadata from Google Maps"
    );
    Ok(passed())
}

async fn test_code_execution_plus_function_tool() -> anyhow::Result<ScenarioOutcome> {
    separator("4. Code execution + function tool coexistence");
    let model = scenario_model("GEMINI_CODE_EXECUTION_MODEL", MODEL_NAME);

    let summary = match run_agent_turn(
        "gemini-code-execution",
        &model,
        "gemini-code-execution",
        "You are a computational agent. Use Gemini code execution for calculations, then call record_tool_status.",
        vec![Arc::new(GeminiCodeExecutionTool::new()), note_tool()],
        "Use Gemini code execution to calculate the first 10 Fibonacci numbers. After that, call record_tool_status with tool_name 'code_execution' and the 10th value.",
    )
    .await
    {
        Ok(summary) => summary,
        Err(error) => {
            if let Some(reason) =
                skip_known_gemini_incompatibility(&error, "code_execution", &model)
            {
                return skipped(reason);
            }
            return Err(error);
        }
    };

    assert!(
        saw_kind(&summary, "executable_code") || saw_kind(&summary, "code_execution_result"),
        "expected code execution protocol parts"
    );
    Ok(passed())
}

async fn test_file_search_plus_function_tool() -> anyhow::Result<ScenarioOutcome> {
    separator("5. File search + function tool coexistence");
    let model = scenario_model("GEMINI_FILE_SEARCH_MODEL", MODEL_NAME);

    let mut store_names = csv_env("GEMINI_FILE_SEARCH_STORES");
    if store_names.is_empty() {
        if let Some(single) = optional_env("GEMINI_FILE_SEARCH_STORE") {
            store_names.push(single);
        }
    }
    if store_names.is_empty() {
        return skipped(
            "set GEMINI_FILE_SEARCH_STORE or GEMINI_FILE_SEARCH_STORES to run the file-search scenario",
        );
    }

    let prompt = optional_env("GEMINI_FILE_SEARCH_PROMPT").unwrap_or_else(|| {
        "Use Gemini File Search to inspect the indexed content and tell me one concrete fact it contains. After that, call record_tool_status with tool_name 'file_search'."
            .to_string()
    });

    let summary = match run_agent_turn(
        "gemini-file-search",
        &model,
        "gemini-file-search",
        "You can use Gemini File Search to retrieve indexed documents, then call record_tool_status.",
        vec![Arc::new(GeminiFileSearchTool::new(store_names)), note_tool()],
        &prompt,
    )
    .await
    {
        Ok(summary) => summary,
        Err(error) => {
            if let Some(reason) =
                skip_known_gemini_incompatibility(&error, "file_search", &model)
            {
                return skipped(reason);
            }
            return Err(error);
        }
    };

    assert!(
        summary.saw_text || summary.saw_provider_metadata || summary.saw_function_call,
        "expected output from the file search scenario"
    );
    Ok(passed())
}

async fn test_computer_use_invocation() -> anyhow::Result<ScenarioOutcome> {
    separator("6. Computer use invocation");
    let model = scenario_model("GEMINI_COMPUTER_USE_MODEL", COMPUTER_USE_MODEL_NAME);

    #[derive(schemars::JsonSchema, serde::Serialize)]
    struct OpenWebBrowserArgs {
        url: Option<String>,
    }

    async fn open_web_browser(
        _ctx: Arc<dyn ToolContext>,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let url = args["url"]
            .as_str()
            .unwrap_or("https://www.rust-lang.org")
            .to_string();
        Ok(json!({
            "url": url,
            "current_url": url,
            "title": "Rust Programming Language",
        }))
    }

    let summary = match run_agent_turn(
        "gemini-computer-use",
        &model,
        "gemini-computer-use",
        "You can use Gemini computer use. For this demo, stop after the first browser-opening action and do not continue with additional UI steps.",
        vec![
            Arc::new(GeminiComputerUseTool::new(GeminiComputerEnvironment::Browser)),
            Arc::new(
                FunctionTool::new(
                    "open_web_browser",
                    "Open a browser window and return the current URL.",
                    open_web_browser,
                )
                .with_parameters_schema::<OpenWebBrowserArgs>(),
            ),
        ],
        "Use Gemini computer use to open https://www.rust-lang.org in a browser and stop after the first browser-opening action.",
    )
    .await
    {
        Ok(summary) => summary,
        Err(error) => {
            if let Some(reason) =
                skip_known_gemini_incompatibility(&error, "computer_use", &model)
            {
                return skipped(reason);
            }
            return Err(error);
        }
    };

    assert!(summary.saw_function_call, "expected a predefined computer-use function call");
    assert!(summary.saw_function_response, "expected a computer-use function response");
    Ok(passed())
}

async fn test_multi_turn_mixed_tools() -> anyhow::Result<ScenarioOutcome> {
    separator("7. Multi-turn: native tool first, function tool second");

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

    let model_name = scenario_model("GEMINI_MULTI_TURN_MODEL", MODEL_NAME);
    let model = Arc::new(GeminiModel::new(api_key(), &model_name)?);
    let capital_tool: Arc<dyn Tool> = Arc::new(
        FunctionTool::new("get_capital", "Get the capital city of a country.", get_capital)
            .with_parameters_schema::<CapitalArgs>(),
    );
    let agent = Arc::new(
        LlmAgentBuilder::new("gemini-multi-turn")
            .instruction(
                "Use Google Search for current information. Use get_capital for capital-city questions.",
            )
            .model(model)
            .tool(Arc::new(GoogleSearchTool::new()))
            .tool(capital_tool)
            .build()?,
    );

    let runner = make_runner(agent, "gemini-multi-turn").await?;
    let user_id = UserId::new("user")?;
    let session_id = SessionId::new("gemini-multi-turn")?;

    let mut turn_one = match runner
        .run(
            user_id.clone(),
            session_id.clone(),
            Content::new("user").with_text("Search the web for the current population of Kenya."),
        )
        .await
    {
        Ok(stream) => stream,
        Err(error) => {
            if let Some(reason) =
                skip_known_gemini_incompatibility(&error, "multi_turn_native_tool", &model_name)
            {
                return skipped(reason);
            }
            return Err(error.into());
        }
    };

    let mut saw_native_tool = false;
    while let Some(event) = turn_one.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::ServerToolCall { server_tool_call } => {
                        println!(
                            "  → ServerToolCall: {}",
                            gemini_server_tool_kind(server_tool_call)
                        );
                        println!(
                            "    thought_signature: {}",
                            sig_display(&server_tool_sig(server_tool_call))
                        );
                        saw_native_tool = true;
                    }
                    Part::Text { text } if !text.trim().is_empty() => print!("{text}"),
                    Part::Thinking { signature, .. } => {
                        println!("  💭 Thinking (signature: {})", sig_display(signature));
                    }
                    _ => {}
                }
            }
        }
        print_grounding(&event);
    }
    println!();

    let mut turn_two = match runner
        .run(
            user_id,
            session_id,
            Content::new("user")
                .with_text("What is the capital of Kenya? Use the get_capital tool."),
        )
        .await
    {
        Ok(stream) => stream,
        Err(error) => {
            if let Some(reason) =
                skip_known_gemini_incompatibility(&error, "multi_turn_function_tool", &model_name)
            {
                return skipped(reason);
            }
            return Err(error.into());
        }
    };

    let mut saw_function_call = false;
    let mut saw_function_response = false;
    while let Some(event) = turn_two.next().await {
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
                    Part::Text { text } if !text.trim().is_empty() => print!("{text}"),
                    Part::Thinking { signature, .. } => {
                        println!("  💭 Thinking (signature: {})", sig_display(signature));
                    }
                    _ => {}
                }
            }
        }
        print_grounding(&event);
    }
    println!();

    assert!(saw_native_tool, "expected the native Gemini tool to run in turn one");
    assert!(
        saw_function_call && saw_function_response,
        "expected the function tool round-trip in turn two"
    );
    Ok(passed())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_dotenv();

    println!("Gemini Native Tools — Live Agentic Example Matrix");
    println!("=================================================");
    println!("Default model: {}\n", default_model_name());

    let scenarios: Vec<(&str, ScenarioFn)> = vec![
        ("Google Search + function tool", || Box::pin(test_google_search_plus_function_tool())),
        ("URL context + function tool", || Box::pin(test_url_context_plus_function_tool())),
        ("Google Maps + function tool", || Box::pin(test_google_maps_plus_function_tool())),
        ("Code execution + function tool", || Box::pin(test_code_execution_plus_function_tool())),
        ("File search + function tool", || Box::pin(test_file_search_plus_function_tool())),
        ("Computer use invocation", || Box::pin(test_computer_use_invocation())),
        ("Multi-turn native tool then function tool", || Box::pin(test_multi_turn_mixed_tools())),
    ];

    let mut passed_count = 0usize;
    let mut skipped_count = 0usize;
    let mut failed_count = 0usize;

    for (name, run) in scenarios {
        match run().await {
            Ok(ScenarioOutcome::Passed) => passed_count += 1,
            Ok(ScenarioOutcome::Skipped(reason)) => {
                println!("↷ {name}: {reason}");
                skipped_count += 1;
            }
            Err(error) => {
                eprintln!("\n✗ {name} FAILED: {error:#}");
                failed_count += 1;
            }
        }
    }

    separator("Summary");
    println!("  passed:  {passed_count}");
    println!("  skipped: {skipped_count}");
    println!("  failed:  {failed_count}");
    if failed_count > 0 {
        std::process::exit(1);
    }
    Ok(())
}
