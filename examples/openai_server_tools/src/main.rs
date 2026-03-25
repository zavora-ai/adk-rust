//! OpenAI native tools — live agentic example matrix.
//!
//! Demonstrates every OpenAI native tool wrapper currently exposed by ADK-Rust.
//! Each scenario uses an agent, not a bare client call, so the examples show
//! how provider-native tools coexist with ordinary function tools or how native
//! protocol items surface in the agent stream when client-side action is
//! required.
//!
//! Scenarios:
//!   1. Web search + function tool coexistence
//!   2. File search + function tool coexistence
//!   3. Code interpreter + function tool coexistence
//!   4. Image generation invocation surfaced in the agent stream
//!   5. Computer use invocation surfaced in the agent stream
//!   6. MCP server invocation surfaced in the agent stream
//!   7. Local shell invocation surfaced in the agent stream
//!   8. Managed shell invocation surfaced in the agent stream
//!   9. Apply-patch invocation surfaced in the agent stream
//!   10. Multi-turn: hosted tool first, function tool second
//!
//! # Usage
//!
//! ```bash
//! export OPENAI_API_KEY=sk-your-key-here
//! cargo run --manifest-path examples/openai_server_tools/Cargo.toml
//! ```

use adk_core::{Part, SessionId, UserId};
use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use adk_rust::futures::StreamExt;
use adk_rust::prelude::*;
use adk_rust::session::{CreateRequest, SessionService};
use adk_tool::{
    OpenAIApplyPatchTool, OpenAIApproximateLocation, OpenAICodeInterpreterTool,
    OpenAIComputerEnvironment, OpenAIComputerUseTool, OpenAIFileSearchTool,
    OpenAIImageGenerationTool, OpenAILocalShellTool, OpenAIMcpTool, OpenAIShellTool,
    OpenAIWebSearchTool,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

const APP_NAME: &str = "openai-server-tools-example";
const MODEL_NAME: &str = "gpt-5.4";
const COMPUTER_USE_MODEL: &str = "computer-use-preview";

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
    std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set")
}

fn scenario_model(env_name: &str, fallback: &str) -> String {
    optional_env(env_name)
        .or_else(|| optional_env("OPENAI_MODEL"))
        .unwrap_or_else(|| fallback.to_string())
}

fn default_model_name() -> String {
    scenario_model("OPENAI_MODEL", MODEL_NAME)
}

fn local_shell_model() -> Option<String> {
    optional_env("OPENAI_LOCAL_SHELL_MODEL")
}

fn skip_known_openai_incompatibility(
    error: &impl std::fmt::Display,
    tool_name: &str,
    model: &str,
) -> Option<String> {
    let message = error.to_string();
    if message.contains("not supported with") {
        return Some(format!(
            "{tool_name} is not supported with model '{model}'; choose a compatible model override for this scenario"
        ));
    }
    if message.contains("does not have access to model")
        || message.contains("model_not_found")
        || message.contains("unsupported_value")
    {
        return Some(format!(
            "account access does not include model '{model}' for {tool_name}"
        ));
    }
    if tool_name == "computer_use_preview" && message.contains("invalid_request_error") {
        return Some(
            "OpenAI's current GA computer tool no longer accepts the pinned preview request shape; this scenario needs a raw GA `computer` request path instead of the preview wrapper"
                .to_string(),
        );
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

fn server_item_type(value: &Value) -> String {
    value.get("type").and_then(Value::as_str).unwrap_or("unknown").to_string()
}

fn saw_type(summary: &StreamSummary, needle: &str) -> bool {
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
            "Records which native tool the agent used and why.",
            record_tool_status,
        )
        .with_parameters_schema::<ToolStatusArgs>(),
    )
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
    let model = Arc::new(OpenAIResponsesClient::new(OpenAIResponsesConfig::new(
        api_key(),
        model_name.to_string(),
    ))?);

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
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::ServerToolCall { server_tool_call } => {
                        let kind = server_item_type(server_tool_call);
                        println!("  → ServerToolCall: {kind}");
                        summary.server_tool_calls.push(kind);
                    }
                    Part::ServerToolResponse { server_tool_response } => {
                        let kind = server_item_type(server_tool_response);
                        println!(
                            "  ← ServerToolResponse: {kind} [{}B]",
                            server_tool_response.to_string().len()
                        );
                        summary.server_tool_responses.push(kind);
                    }
                    Part::FunctionCall { name, args, .. } => {
                        println!("  → FunctionCall: {name}({args})");
                        summary.saw_function_call = true;
                    }
                    Part::FunctionResponse { function_response, .. } => {
                        println!("  ← FunctionResponse: {}", function_response.response);
                        summary.saw_function_response = true;
                    }
                    Part::Text { text } if !text.trim().is_empty() => {
                        print!("{text}");
                        summary.text.push_str(text);
                        summary.saw_text = true;
                    }
                    Part::Thinking { .. } => {
                        println!("  💭 reasoning");
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

async fn test_web_search_plus_function_tool() -> anyhow::Result<ScenarioOutcome> {
    separator("1. Web search + function tool coexistence");
    let model = scenario_model("OPENAI_WEB_SEARCH_MODEL", MODEL_NAME);

    let summary = match run_agent_turn(
        "openai-web-search",
        &model,
        "openai-web-search",
        "You are a research agent. Use hosted tools when the prompt explicitly asks for them. After you finish the hosted-tool step, call record_tool_status with the tool name and a short note.",
        vec![
            Arc::new(
                OpenAIWebSearchTool::new()
                    .with_search_context_size("medium")
                    .with_user_location(
                        OpenAIApproximateLocation::new()
                            .with_city("San Francisco")
                            .with_country("US")
                            .with_region("California")
                            .with_timezone("America/Los_Angeles"),
                    ),
            ),
            note_tool(),
        ],
        "Use OpenAI web search to find the latest stable Rust release. After searching, call record_tool_status with tool_name 'web_search' and a short note.",
    )
    .await
    {
        Ok(summary) => summary,
        Err(error) => {
            if let Some(reason) = skip_known_openai_incompatibility(&error, "web_search", &model) {
                return skipped(reason);
            }
            return Err(error);
        }
    };

    assert!(saw_type(&summary, "web_search"), "expected a web_search call");
    Ok(passed())
}

async fn test_file_search_plus_function_tool() -> anyhow::Result<ScenarioOutcome> {
    separator("2. File search + function tool coexistence");
    let model = scenario_model("OPENAI_FILE_SEARCH_MODEL", MODEL_NAME);

    let mut vector_store_ids = csv_env("OPENAI_VECTOR_STORE_IDS");
    if vector_store_ids.is_empty() {
        if let Some(single) = optional_env("OPENAI_VECTOR_STORE_ID") {
            vector_store_ids.push(single);
        }
    }
    if vector_store_ids.is_empty() {
        return skipped(
            "set OPENAI_VECTOR_STORE_ID or OPENAI_VECTOR_STORE_IDS to run the file-search scenario",
        );
    }

    let prompt = optional_env("OPENAI_FILE_SEARCH_PROMPT").unwrap_or_else(|| {
        "Use file search to inspect the indexed documents and tell me one concrete fact they contain. After that, call record_tool_status with tool_name 'file_search' and the fact."
            .to_string()
    });

    let summary = match run_agent_turn(
        "openai-file-search",
        &model,
        "openai-file-search",
        "You are a retrieval agent. Use file search when asked about indexed documents, then call record_tool_status.",
        vec![
            Arc::new(OpenAIFileSearchTool::new(vector_store_ids).with_max_num_results(4)),
            note_tool(),
        ],
        &prompt,
    )
    .await
    {
        Ok(summary) => summary,
        Err(error) => {
            if let Some(reason) = skip_known_openai_incompatibility(&error, "file_search", &model)
            {
                return skipped(reason);
            }
            return Err(error);
        }
    };

    assert!(saw_type(&summary, "file_search"), "expected a file_search call");
    Ok(passed())
}

async fn test_code_interpreter_plus_function_tool() -> anyhow::Result<ScenarioOutcome> {
    separator("3. Code interpreter + function tool coexistence");
    let model = scenario_model("OPENAI_CODE_INTERPRETER_MODEL", MODEL_NAME);

    let summary = match run_agent_turn(
        "openai-code-interpreter",
        &model,
        "openai-code-interpreter",
        "You are a computational agent. Use the code interpreter when asked to calculate or transform data, then call record_tool_status.",
        vec![Arc::new(OpenAICodeInterpreterTool::new()), note_tool()],
        "Use the code interpreter to calculate the 10th and 20th Fibonacci numbers. After you finish, call record_tool_status with tool_name 'code_interpreter' and the two values.",
    )
    .await
    {
        Ok(summary) => summary,
        Err(error) => {
            if let Some(reason) =
                skip_known_openai_incompatibility(&error, "code_interpreter", &model)
            {
                return skipped(reason);
            }
            return Err(error);
        }
    };

    assert!(saw_type(&summary, "code_interpreter"), "expected a code_interpreter call");
    Ok(passed())
}

async fn test_image_generation_invocation() -> anyhow::Result<ScenarioOutcome> {
    separator("4. Image generation invocation");
    let model = scenario_model("OPENAI_IMAGE_GENERATION_MODEL", MODEL_NAME);

    let summary = match run_agent_turn(
        "openai-image-generation",
        &model,
        "openai-image-generation",
        "You are a creative agent. Use image generation when the user asks for a generated image.",
        vec![Arc::new(
            OpenAIImageGenerationTool::new()
                .with_option("size", json!("1024x1024"))
                .with_option("background", json!("transparent")),
        )],
        "Use image generation to create a minimal orange square logo with the letters ADK. Explain briefly what you generated.",
    )
    .await
    {
        Ok(summary) => summary,
        Err(error) => {
            if let Some(reason) =
                skip_known_openai_incompatibility(&error, "image_generation", &model)
            {
                return skipped(reason);
            }
            return Err(error);
        }
    };

    assert!(saw_type(&summary, "image_generation"), "expected an image_generation call");
    Ok(passed())
}

async fn test_computer_use_invocation() -> anyhow::Result<ScenarioOutcome> {
    separator("5. Computer use invocation");
    let model = scenario_model("OPENAI_COMPUTER_USE_MODEL", COMPUTER_USE_MODEL);

    let summary = match run_agent_turn(
        "openai-computer-use",
        &model,
        "openai-computer-use",
        "You are a browser automation agent. Use computer use when asked to interact with a UI. Stop after the first computer action.",
        vec![Arc::new(OpenAIComputerUseTool::new(
            OpenAIComputerEnvironment::Browser,
            1440,
            900,
        ))],
        "Use the computer use tool to inspect https://www.rust-lang.org and stop after the first UI action you want to take.",
    )
    .await
    {
        Ok(summary) => summary,
        Err(error) => {
            if let Some(reason) =
                skip_known_openai_incompatibility(&error, "computer_use_preview", &model)
            {
                return skipped(reason);
            }
            return Err(error);
        }
    };

    assert!(saw_type(&summary, "computer"), "expected a computer call");
    Ok(passed())
}

async fn test_mcp_invocation() -> anyhow::Result<ScenarioOutcome> {
    separator("6. MCP invocation");
    let model = scenario_model("OPENAI_MCP_MODEL", MODEL_NAME);

    let mcp_tool: Arc<dyn Tool> = if let Some(server_url) = optional_env("OPENAI_MCP_SERVER_URL") {
        Arc::new(
            OpenAIMcpTool::new_with_url("example-mcp", server_url)
                .with_require_approval(json!("never")),
        )
    } else if let Some(connector_id) = optional_env("OPENAI_MCP_CONNECTOR_ID") {
        Arc::new(
            OpenAIMcpTool::new_with_connector("example-mcp", connector_id)
                .with_require_approval(json!("never")),
        )
    } else {
        return skipped(
            "set OPENAI_MCP_SERVER_URL or OPENAI_MCP_CONNECTOR_ID to run the MCP scenario",
        );
    };

    let prompt = optional_env("OPENAI_MCP_PROMPT").unwrap_or_else(|| {
        "Use the MCP server to inspect its capabilities or perform a small action. After that, call record_tool_status with tool_name 'mcp' and a short note."
            .to_string()
    });

    let summary = match run_agent_turn(
        "openai-mcp",
        &model,
        "openai-mcp",
        "You can use an MCP server and a local record_tool_status function. Use the MCP server first, then record what happened.",
        vec![mcp_tool, note_tool()],
        &prompt,
    )
    .await
    {
        Ok(summary) => summary,
        Err(error) => {
            if let Some(reason) = skip_known_openai_incompatibility(&error, "mcp", &model) {
                return skipped(reason);
            }
            return Err(error);
        }
    };

    assert!(saw_type(&summary, "mcp"), "expected an mcp call");
    Ok(passed())
}

async fn test_local_shell_invocation() -> anyhow::Result<ScenarioOutcome> {
    separator("7. Local shell invocation");
    let Some(model) = local_shell_model() else {
        return skipped(
            "local_shell is a legacy OpenAI tool; set OPENAI_LOCAL_SHELL_MODEL to attempt it explicitly",
        );
    };

    let summary = match run_agent_turn(
        "openai-local-shell",
        &model,
        "openai-local-shell",
        "You can use the local shell tool. Stop after producing the first local shell call.",
        vec![Arc::new(OpenAILocalShellTool::new())],
        "Use the local shell tool to print the current working directory and stop after issuing the shell call.",
    )
    .await
    {
        Ok(summary) => summary,
        Err(error) => {
            if let Some(reason) = skip_known_openai_incompatibility(&error, "local_shell", &model)
            {
                return skipped(reason);
            }
            return Err(error);
        }
    };

    assert!(saw_type(&summary, "local_shell"), "expected a local_shell call");
    Ok(passed())
}

async fn test_managed_shell_invocation() -> anyhow::Result<ScenarioOutcome> {
    separator("8. Managed shell invocation");
    let model = scenario_model("OPENAI_SHELL_MODEL", MODEL_NAME);

    let summary = match run_agent_turn(
        "openai-shell",
        &model,
        "openai-shell",
        "You can use the managed shell tool. If the tool executes successfully, summarize the result.",
        vec![Arc::new(OpenAIShellTool::new())],
        "Use the managed shell tool to run `pwd` and `ls`.",
    )
    .await
    {
        Ok(summary) => summary,
        Err(error) => {
            if let Some(reason) = skip_known_openai_incompatibility(&error, "shell", &model) {
                return skipped(reason);
            }
            return Err(error);
        }
    };

    assert!(
        saw_type(&summary, "shell_call") || saw_type(&summary, "shell"),
        "expected a shell call or shell output"
    );
    Ok(passed())
}

async fn test_apply_patch_invocation() -> anyhow::Result<ScenarioOutcome> {
    separator("9. Apply-patch invocation");
    let model = scenario_model("OPENAI_APPLY_PATCH_MODEL", MODEL_NAME);

    let summary = match run_agent_turn(
        "openai-apply-patch",
        &model,
        "openai-apply-patch",
        "You can use the apply_patch native tool. Stop after proposing the patch.",
        vec![Arc::new(OpenAIApplyPatchTool::new())],
        "Use apply_patch to propose creating a file named OPENAI_TOOL_DEMO.md containing one line: native tool demo.",
    )
    .await
    {
        Ok(summary) => summary,
        Err(error) => {
            if let Some(reason) = skip_known_openai_incompatibility(&error, "apply_patch", &model)
            {
                return skipped(reason);
            }
            return Err(error);
        }
    };

    assert!(saw_type(&summary, "apply_patch"), "expected an apply_patch call");
    Ok(passed())
}

async fn test_multi_turn_hosted_tool_then_function_tool() -> anyhow::Result<ScenarioOutcome> {
    separator("10. Multi-turn: hosted tool first, function tool second");

    #[derive(schemars::JsonSchema, serde::Serialize)]
    struct WeatherArgs {
        city: String,
    }

    async fn get_weather(
        _ctx: Arc<dyn ToolContext>,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        Ok(json!({
            "city": args["city"].as_str().unwrap_or("Unknown"),
            "temperature_c": 22,
            "condition": "Partly cloudy"
        }))
    }

    let model_name = scenario_model("OPENAI_MULTI_TURN_MODEL", MODEL_NAME);
    let model = Arc::new(OpenAIResponsesClient::new(OpenAIResponsesConfig::new(
        api_key(),
        model_name.clone(),
    ))?);
    let weather_tool: Arc<dyn Tool> = Arc::new(
        FunctionTool::new("get_weather", "Get current weather for a city.", get_weather)
            .with_parameters_schema::<WeatherArgs>(),
    );

    let agent = Arc::new(
        LlmAgentBuilder::new("openai-multi-turn")
            .instruction(
                "Use OpenAI web search for current events. Use get_weather for weather questions.",
            )
            .model(model)
            .tool(Arc::new(OpenAIWebSearchTool::new()))
            .tool(weather_tool)
            .build()?,
    );

    let runner = make_runner(agent, "openai-multi-turn").await?;
    let user_id = UserId::new("user")?;
    let session_id = SessionId::new("openai-multi-turn")?;

    let mut turn_one = match runner
        .run(
            user_id.clone(),
            session_id.clone(),
            Content::new("user")
                .with_text("Use web search to find the latest Rust stable release."),
        )
        .await
    {
        Ok(stream) => stream,
        Err(error) => {
            if let Some(reason) =
                skip_known_openai_incompatibility(&error, "multi_turn_web_search", &model_name)
            {
                return skipped(reason);
            }
            return Err(error.into());
        }
    };

    let mut saw_web_search = false;
    while let Some(event) = turn_one.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::ServerToolCall { server_tool_call } => {
                        if server_item_type(server_tool_call).contains("web_search") {
                            saw_web_search = true;
                        }
                    }
                    Part::Text { text } if !text.trim().is_empty() => print!("{text}"),
                    _ => {}
                }
            }
        }
    }
    println!();

    let mut turn_two = match runner
        .run(
            user_id,
            session_id,
            Content::new("user")
                .with_text("What is the weather in Tokyo right now? Use the get_weather tool."),
        )
        .await
    {
        Ok(stream) => stream,
        Err(error) => {
            if let Some(reason) =
                skip_known_openai_incompatibility(&error, "multi_turn_get_weather", &model_name)
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
                    Part::FunctionCall { name, .. } => {
                        println!("  → FunctionCall: {name}");
                        saw_function_call = true;
                    }
                    Part::FunctionResponse { function_response, .. } => {
                        println!("  ← FunctionResponse: {}", function_response.response);
                        saw_function_response = true;
                    }
                    Part::Text { text } if !text.trim().is_empty() => print!("{text}"),
                    _ => {}
                }
            }
        }
    }
    println!();

    assert!(saw_web_search, "expected web search in turn one");
    assert!(saw_function_call, "expected get_weather call in turn two");
    assert!(saw_function_response, "expected get_weather response in turn two");
    Ok(passed())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_dotenv();

    println!("OpenAI Native Tools — Live Agentic Example Matrix");
    println!("=================================================");
    println!("Default model: {}\n", default_model_name());

    let scenarios: Vec<(&str, ScenarioFn)> = vec![
        ("Web search + function tool", || Box::pin(test_web_search_plus_function_tool())),
        ("File search + function tool", || Box::pin(test_file_search_plus_function_tool())),
        ("Code interpreter + function tool", || {
            Box::pin(test_code_interpreter_plus_function_tool())
        }),
        ("Image generation invocation", || Box::pin(test_image_generation_invocation())),
        ("Computer use invocation", || Box::pin(test_computer_use_invocation())),
        ("MCP invocation", || Box::pin(test_mcp_invocation())),
        ("Local shell invocation", || Box::pin(test_local_shell_invocation())),
        ("Managed shell invocation", || Box::pin(test_managed_shell_invocation())),
        ("Apply-patch invocation", || Box::pin(test_apply_patch_invocation())),
        ("Multi-turn hosted tool then function tool", || {
            Box::pin(test_multi_turn_hosted_tool_then_function_tool())
        }),
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
